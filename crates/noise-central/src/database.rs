use anyhow::Context;
use deadpool_postgres::{Manager, ManagerConfig, Pool, RecyclingMethod};
use tokio_postgres::NoTls;

use crate::config::CentralConfig;

#[derive(Clone)]
pub struct Database {
    pub pool: Pool,
}

impl Database {
    pub async fn connect(config: &CentralConfig) -> anyhow::Result<Self> {
        let mut postgres = tokio_postgres::Config::new();
        postgres
            .host(&config.database_host)
            .port(config.database_port)
            .dbname(&config.database_name)
            .user(&config.database_user)
            .password(&config.database_password);

        let manager = Manager::from_config(
            postgres,
            NoTls,
            ManagerConfig {
                recycling_method: RecyclingMethod::Verified,
            },
        );
        let pool = Pool::builder(manager)
            .max_size(config.database_pool_size)
            .build()
            .context("could not create PostgreSQL pool")?;
        let database = Self { pool };
        database.verify_schema().await?;
        Ok(database)
    }

    async fn verify_schema(&self) -> anyhow::Result<()> {
        let client = self
            .pool
            .get()
            .await
            .context("could not connect to PostgreSQL")?;
        let row = client
            .query_opt(
                "SELECT version FROM noise.schema_migrations ORDER BY version DESC LIMIT 1",
                &[],
            )
            .await
            .context("canonical noise schema is not installed")?
            .context("canonical noise schema has no migration record")?;
        let version: i32 = row.get(0);
        if version != 1 {
            anyhow::bail!("unsupported canonical schema version {version}");
        }
        Ok(())
    }
}
