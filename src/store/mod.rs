pub mod sqlite;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoreConfig {
    pub database_path: String,
}

impl StoreConfig {
    pub fn sqlite(database_path: impl Into<String>) -> Self {
        Self {
            database_path: database_path.into(),
        }
    }
}
