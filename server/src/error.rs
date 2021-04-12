use std::sync::Arc;

use pliantdb_core::networking::{self, fabruic};
use pliantdb_local::core::{self, schema};

/// An error occurred while interacting with a [`Server`](crate::Server).
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// An invalid database name was specified. See
    /// [`ServerConnection::create_database()`](pliantdb_core::networking::ServerConnection::create_database)
    /// for database name requirements.
    #[error("invalid database name: {0}")]
    InvalidDatabaseName(String),

    /// The database name given was not found.
    #[error("database '{0}' was not found")]
    DatabaseNotFound(String),

    /// The database name already exists.
    #[error("a database with name '{0}' already exists")]
    DatabaseNameAlreadyTaken(String),

    /// An error occurred from the QUIC transport layer.
    #[error("a networking error occurred: '{0}'")]
    Transport(#[from] fabruic::Error),

    #[cfg(feature = "websockets")]
    /// An error occurred from the Websocket transport layer.
    #[error("a websocket error occurred: '{0}'")]
    Websocket(#[from] tokio_tungstenite::tungstenite::Error),

    /// An error occurred from IO
    #[error("a networking error occurred: '{0}'")]
    Io(#[from] tokio::io::Error),

    /// An error occurred while processing a request
    #[error("an error occurred processing a request: '{0}'")]
    Request(Arc<anyhow::Error>),

    /// The database named `database_name` was created with a different schema
    /// (`stored_schema`) than provided (`schema`).
    #[error(
        "database '{database_name}' was created with schema '{stored_schema}', not '{schema}'"
    )]
    SchemaMismatch {
        /// The name of the database being accessed.
        database_name: String,

        /// The schema provided for the database.
        schema: schema::Id,

        /// The schema stored for the database.
        stored_schema: schema::Id,
    },

    /// The [`schema::Id`] returned has already been registered with this server.
    #[error("schema '{0}' was already registered")]
    SchemaAlreadyRegistered(schema::Id),

    /// An error occurred from within the schema.
    #[error("error from core {0}")]
    Core(#[from] core::Error),

    /// An error occurred while interacting with a local database.
    #[error("an error occurred interacting with a database: {0}")]
    Storage(#[from] pliantdb_local::Error),
}

impl From<Error> for core::Error {
    fn from(other: Error) -> Self {
        // without it, there's no way to get this to_string() easily.
        #[allow(clippy::clippy::match_wildcard_for_single_variants)]
        match other {
            Error::Storage(storage) => Self::Storage(storage.to_string()),
            Error::Core(core) => core,
            Error::Io(io) => Self::Io(io.to_string()),
            Error::Transport(networking) => Self::Transport(networking.to_string()),
            #[cfg(feature = "websockets")]
            Error::Websocket(err) => Self::Websocket(err.to_string()),
            Error::InvalidDatabaseName(name) => {
                Self::Networking(networking::Error::InvalidDatabaseName(name))
            }
            Error::DatabaseNotFound(name) => {
                Self::Networking(networking::Error::DatabaseNotFound(name))
            }
            Error::DatabaseNameAlreadyTaken(name) => {
                Self::Networking(networking::Error::DatabaseNameAlreadyTaken(name))
            }
            Error::Request(err) => Self::Server(err.to_string()),
            Error::SchemaMismatch {
                database_name,
                schema,
                stored_schema,
            } => Self::Networking(networking::Error::SchemaMismatch {
                database_name,
                schema,
                stored_schema,
            }),
            Error::SchemaAlreadyRegistered(id) => {
                Self::Networking(networking::Error::SchemaAlreadyRegistered(id))
            }
        }
    }
}

pub trait ResultExt<R> {
    fn map_err_to_core(self) -> Result<R, core::Error>
    where
        Self: Sized;
}

impl<R> ResultExt<R> for Result<R, Error> {
    fn map_err_to_core(self) -> Result<R, core::Error>
    where
        Self: Sized,
    {
        self.map_err(core::Error::from)
    }
}

#[cfg(feature = "websockets")]
impl From<bincode::Error> for Error {
    fn from(other: bincode::Error) -> Self {
        Self::Core(core::Error::Websocket(format!(
            "error deserializing message: {:?}",
            other
        )))
    }
}
