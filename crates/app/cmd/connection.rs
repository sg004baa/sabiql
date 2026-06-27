use std::sync::Arc;

use color_eyre::eyre::Result;
use tokio::sync::mpsc;

use crate::cmd::cache::TtlCache;
use crate::cmd::effect::Effect;
use crate::domain::DatabaseMetadata;
use crate::domain::connection::ConnectionProfile;
use crate::ports::outbound::{ConnectionStore, DsnBuilder, MetadataProvider};
use crate::update::action::{Action, ConnectionTarget};

pub(crate) async fn run(
    effect: Effect,
    action_tx: &mpsc::Sender<Action>,
    dsn_builder: &Arc<dyn DsnBuilder>,
    metadata_provider: &Arc<dyn MetadataProvider>,
    metadata_cache: &TtlCache<String, Arc<DatabaseMetadata>>,
    connection_store: &Arc<dyn ConnectionStore>,
) -> Result<()> {
    match effect {
        Effect::SaveAndConnect {
            id,
            name,
            host,
            port,
            database,
            user,
            password,
            ssl_mode,
            database_type,
        } => {
            let profile = match id {
                Some(existing_id) => {
                    match ConnectionProfile::with_id(
                        existing_id,
                        name,
                        host,
                        port,
                        database,
                        user,
                        password,
                        ssl_mode,
                        database_type,
                    ) {
                        Ok(profile) => profile,
                        Err(error) => {
                            action_tx
                                .blocking_send(Action::ConnectionSaveFailed(error.into()))
                                .ok();
                            return Ok(());
                        }
                    }
                }
                None => {
                    match ConnectionProfile::new(
                        name,
                        host,
                        port,
                        database,
                        user,
                        password,
                        ssl_mode,
                        database_type,
                    ) {
                        Ok(profile) => profile,
                        Err(error) => {
                            action_tx
                                .blocking_send(Action::ConnectionSaveFailed(error.into()))
                                .ok();
                            return Ok(());
                        }
                    }
                }
            };
            let id = profile.id.clone();
            let dsn = dsn_builder.build_dsn(&profile);
            let name = profile.name.as_str().to_string();
            let store = Arc::clone(connection_store);
            let tx = action_tx.clone();
            let provider = Arc::clone(metadata_provider);
            let cache = metadata_cache.clone();

            tokio::spawn(async move {
                match provider.fetch_metadata(&dsn).await {
                    Ok(metadata) => {
                        cache.set(dsn.clone(), Arc::new(metadata)).await;
                        match store.save(&profile) {
                            Ok(()) => {
                                tx.send(Action::ConnectionSaveCompleted(ConnectionTarget {
                                    id,
                                    dsn,
                                    name,
                                }))
                                .await
                                .ok();
                            }
                            Err(error) => {
                                cache.invalidate(&dsn).await;
                                tx.send(Action::ConnectionSaveFailed(error.into()))
                                    .await
                                    .ok();
                            }
                        }
                    }
                    Err(error) => {
                        tx.send(Action::MetadataFailed(error)).await.ok();
                    }
                }
            });
            Ok(())
        }
        _ => unreachable!("connection::run called with non-connection effect"),
    }
}
