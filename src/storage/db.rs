#[cfg(test)]
#[path = "db_test.rs"]
mod db_test;

use std::{borrow::Cow, path::Path, result, sync::Arc};

use libmdbx::{DatabaseFlags, Geometry, WriteFlags, WriteMap};
use serde::de::DeserializeOwned;
use serde::Serialize;

/*
  Low database layer for interaction with libmdbx. The API is supposedly generic enough to easily
  replace the database library with other Berkley-like database implementations.

  Assumptions:
  * The serialization is consistent across code versions (though, not necessarily across machines).
*/

// Maximum number of Sub-Databases.
// TODO(spapini): Get these from configuration, and have a separate test configuration.
const MAX_DBS: usize = 10;
const MIN_SIZE: usize = 1 << 20; // Minimum db size 1MB;
const MAX_SIZE: usize = 1 << 40; // Maximum db size 1TB;
const GROWTH_STEP: isize = 1 << 26; // Growth step 64MB;

// Note that NO_TLS mode is used by default.
type EnvironmentKind = WriteMap;
type Environment = libmdbx::Environment<EnvironmentKind>;

#[derive(thiserror::Error, Debug)]
pub enum DbError {
    #[error(transparent)]
    InnerDbError(#[from] libmdbx::Error),
    #[error(transparent)]
    DeserializationError(#[from] bincode::Error),
}
pub type Result<V> = result::Result<V, DbError>;

/// Opens an MDBX environment and returns a reader and a writer to it.
/// There is a single non clonable writer instance, to make sure there is only one write transaction
///  at any given moment.
#[allow(dead_code)]
pub fn open_env(path: &Path) -> Result<(DbReader, DbWriter)> {
    let env = Arc::new(
        Environment::new()
            .set_geometry(Geometry {
                size: Some(MIN_SIZE..MAX_SIZE),
                growth_step: Some(GROWTH_STEP),
                ..Default::default()
            })
            .set_max_dbs(MAX_DBS)
            .open(path)?,
    );
    Ok((DbReader { env: env.clone() }, DbWriter { env }))
}

#[derive(Clone)]
pub struct DbReader {
    env: Arc<Environment>,
}
pub struct DbWriter {
    env: Arc<Environment>,
}

pub struct DbTransaction<'a, K: libmdbx::TransactionKind> {
    txn: libmdbx::Transaction<'a, K, EnvironmentKind>,
}
pub type DbReadTransaction<'a> = DbTransaction<'a, libmdbx::RO>;
pub type DbWriteTransaction<'a> = DbTransaction<'a, libmdbx::RW>;

pub struct TableIdentifier {
    name: &'static str,
}
pub struct TableHandle<'txn> {
    database: libmdbx::Database<'txn>,
}

impl DbReader {
    pub fn begin_ro_txn(&self) -> Result<DbReadTransaction<'_>> {
        Ok(DbReadTransaction {
            txn: self.env.begin_ro_txn()?,
        })
    }
}
impl DbWriter {
    pub fn begin_rw_txn(&mut self) -> Result<DbWriteTransaction<'_>> {
        Ok(DbWriteTransaction {
            txn: self.env.begin_rw_txn()?,
        })
    }
    pub fn create_table(&mut self, name: &'static str) -> Result<TableIdentifier> {
        let txn = self.env.begin_rw_txn()?;
        txn.create_db(Some(name), DatabaseFlags::empty())?;
        txn.commit()?;
        Ok(TableIdentifier { name })
    }
}

impl<'a, K: libmdbx::TransactionKind> DbTransaction<'a, K> {
    pub fn open_table<'txn>(&'txn self, table_id: &TableIdentifier) -> Result<TableHandle<'txn>> {
        let database = self.txn.open_db(Some(table_id.name))?;
        Ok(TableHandle { database })
    }
    pub fn get<'txn, ValueType: DeserializeOwned>(
        &'txn self,
        table: &TableHandle<'txn>,
        key: &[u8],
    ) -> Result<Option<ValueType>> {
        // TODO: Support zero-copy. This might require a return type of Cow<'txn, ValueType>.
        if let Some(bytes) = self.txn.get::<Cow<'txn, [u8]>>(&table.database, key)? {
            let value = bincode::deserialize::<ValueType>(bytes.as_ref())?;
            Ok(Some(value))
        } else {
            Ok(None)
        }
    }
}
impl<'a> DbWriteTransaction<'a> {
    pub fn upsert<'txn, ValueType: Serialize>(
        &'txn self,
        table: &TableHandle<'txn>,
        key: &[u8],
        value: &ValueType,
    ) -> Result<()> {
        let data = bincode::serialize::<ValueType>(value).unwrap();
        self.txn
            .put(&table.database, key, &data, WriteFlags::UPSERT)?;
        Ok(())
    }
    #[allow(dead_code)]
    pub fn insert<'txn, ValueType: Serialize>(
        &'txn self,
        table: &TableHandle<'txn>,
        key: &[u8],
        value: &ValueType,
    ) -> Result<()> {
        let data = bincode::serialize::<ValueType>(value).unwrap();
        self.txn
            .put(&table.database, key, &data, WriteFlags::NO_OVERWRITE)?;
        Ok(())
    }
    #[allow(dead_code)]
    pub fn delete<'txn, ValueType: Serialize>(
        &'txn self,
        table: &TableHandle<'txn>,
        key: &[u8],
    ) -> Result<()> {
        self.txn.del(&table.database, key, None)?;
        Ok(())
    }
    pub fn commit(self) -> Result<()> {
        self.txn.commit()?;
        Ok(())
    }
}