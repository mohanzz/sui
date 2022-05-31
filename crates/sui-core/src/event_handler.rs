// Copyright (c) 2022, Mysten Labs, Inc.
// SPDX-License-Identifier: Apache-2.0

use crate::authority::{ArcWrapper, AuthorityStore};
use crate::streamer::Streamer;
use futures::future::join_all;
use move_bytecode_utils::module_cache::SyncModuleCache;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use sui_types::{
    error::{SuiError, SuiResult},
    event::{Event, EventEnvelope},
    messages::TransactionEffects,
};
use tokio::sync::mpsc::{self, Sender};

use tracing::{debug, error};

const EVENT_DISPATCH_BUFFER_SIZE: usize = 1000;

pub fn get_unixtime_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("Travelling in time machine")
        .as_millis()
}

pub struct EventHandler {
    module_cache: SyncModuleCache<ArcWrapper<AuthorityStore>>,
    streamer_queue: Sender<EventEnvelope>,
}

impl EventHandler {
    pub fn new(validator_store: Arc<AuthorityStore>) -> Self {
        let (tx, rx) = mpsc::channel::<EventEnvelope>(EVENT_DISPATCH_BUFFER_SIZE);
        Streamer::spawn(rx);
        Self {
            module_cache: SyncModuleCache::new(ArcWrapper(validator_store)),
            streamer_queue: tx,
        }
    }

    pub async fn process_events(&self, effects: &TransactionEffects) {
        let futures = effects.events.iter().map(|e| self.process_event(e));
        let results = join_all(futures).await;
        for r in &results {
            if let Err(e) = r {
                error!(error =? e, "Failed to send EventEnvolope to dispatch");
            }
        }
    }

    pub async fn process_event(&self, event: &Event) -> SuiResult {
        let envolope = match event {
            Event::MoveEvent {
                type_: _,
                contents: _,
            } => {
                debug!(event =? event, "Process MoveEvent.");
                match event.extract_move_struct(&self.module_cache) {
                    Ok(Some(move_struct)) => {
                        let json_value = serde_json::to_value(&move_struct).map_err(|e| {
                            SuiError::ObjectSerializationError {
                                error: e.to_string(),
                            }
                        })?;
                        EventEnvelope::new(get_unixtime_ms(), None, event.clone(), Some(json_value))
                    }
                    Ok(None) => unreachable!("Expect a MoveStruct from a MoveEvent."),
                    Err(e) => return Err(e),
                }
            }
            // TODO add other types of Event
            _ => {
                return Ok(());
            }
        };

        // TODO store events here

        self.streamer_queue
            .send(envolope)
            .await
            .map_err(|e| SuiError::EventFailedToDispatch {
                error: e.to_string(),
            })
    }
}
