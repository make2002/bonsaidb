#![allow(missing_docs)]

use std::path::Path;

use pliantdb_core::{connection::ServerConnection, test_util::BasicSchema};

use crate::{Configuration, Server};

pub const BASIC_SERVER_NAME: &str = "basic-server";

pub async fn initialize_basic_server(path: &Path) -> anyhow::Result<Server> {
    let server = Server::open(path, Configuration::default()).await?;
    server.register_schema::<BasicSchema>().await?;
    server
        .install_self_signed_certificate(BASIC_SERVER_NAME, false)
        .await?;

    server.create_database::<BasicSchema>("tests").await?;

    Ok(server)
}
