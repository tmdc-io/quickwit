// Copyright 2021-Present Datadog, Inc.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::time::{Duration, Instant};
use std::{fmt, mem};

use anyhow::Context;
use async_trait::async_trait;
use bytes::Bytes;
use futures::{FutureExt, StreamExt};
use google_cloud_auth::credentials::{Credentials, service_account};
use google_cloud_pubsub::client::{Subscriber, SubscriptionAdmin};
use google_cloud_pubsub::subscriber::MessageStream;
use quickwit_actors::ActorExitStatus;
use quickwit_common::rand::append_random_suffix;
use quickwit_config::PubSubSourceParams;
use quickwit_metastore::checkpoint::{PartitionId, SourceCheckpoint};
use quickwit_proto::metastore::SourceType;
use quickwit_proto::types::Position;
use serde_json::{Value as JsonValue, json};
use tokio::sync::Mutex;
use tokio::time;
use tracing::{debug, info, warn};

use super::{BATCH_NUM_BYTES_LIMIT, EMIT_BATCHES_TIMEOUT};
use crate::source::{
    BatchBuilder, Source, SourceContext, SourceRuntime, SourceSink, TypedSourceFactory,
};

const DEFAULT_MAX_MESSAGES_PER_PULL: i32 = 1_000;

pub struct GcpPubSubSourceFactory;

#[async_trait]
impl TypedSourceFactory for GcpPubSubSourceFactory {
    type Source = GcpPubSubSource;
    type Params = PubSubSourceParams;

    async fn typed_create_source(
        source_runtime: SourceRuntime,
        source_params: PubSubSourceParams,
    ) -> anyhow::Result<Self::Source> {
        GcpPubSubSource::try_new(source_runtime, source_params).await
    }
}

#[derive(Default)]
pub struct GcpPubSubSourceState {
    /// Number of bytes processed by the source.
    num_bytes_processed: u64,
    /// Number of messages processed by the source.
    num_messages_processed: u64,
    /// Current position of the source, i.e. the position of the last message processed.
    current_position: Position,
    // Number of invalid messages, i.e., that were empty or could not be parsed.
    num_invalid_messages: u64,
    /// Number of time we looped without getting a single message
    num_consecutive_empty_batches: u64,
}

pub struct GcpPubSubSource {
    source_runtime: SourceRuntime,
    subscription_name: String,
    subscription_fqsn: String,
    message_stream: Mutex<MessageStream>,
    state: GcpPubSubSourceState,
    backfill_mode_enabled: bool,
    partition_id: PartitionId,
    max_messages_per_pull: i32,
}

impl fmt::Debug for GcpPubSubSource {
    fn fmt(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter
            .debug_struct("GcpPubSubSource")
            .field("index_id", &self.source_runtime.index_id())
            .field("source_id", &self.source_runtime.source_id())
            .field("subscription", &self.subscription_fqsn)
            .finish()
    }
}

fn fully_qualified_subscription_name(subscription: &str, project_id: Option<&str>) -> String {
    if subscription.starts_with("projects/") {
        subscription.to_string()
    } else if let Some(project_id) = project_id {
        format!("projects/{project_id}/subscriptions/{subscription}")
    } else {
        subscription.to_string()
    }
}

async fn load_credentials(credentials_file: &str) -> anyhow::Result<Credentials> {
    let contents = tokio::fs::read_to_string(credentials_file)
        .await
        .with_context(|| format!("failed to read GCP credentials file `{credentials_file}`"))?;
    let json: serde_json::Value = serde_json::from_str(&contents)
        .with_context(|| format!("failed to parse GCP credentials file `{credentials_file}`"))?;
    service_account::Builder::new(json)
        .build()
        .context("failed to build GCP credentials from service account key")
}

fn build_pubsub_clients(
    credentials: Option<Credentials>,
) -> impl std::future::Future<Output = anyhow::Result<(Subscriber, SubscriptionAdmin)>> {
    async move {
        let mut subscriber_builder = Subscriber::builder();
        let mut admin_builder = SubscriptionAdmin::builder();
        if let Some(credentials) = credentials {
            subscriber_builder = subscriber_builder.with_credentials(credentials.clone());
            admin_builder = admin_builder.with_credentials(credentials);
        }
        let subscriber = subscriber_builder
            .build()
            .await
            .context("failed to create GCP PubSub subscriber client")?;
        let admin = admin_builder
            .build()
            .await
            .context("failed to create GCP PubSub subscription admin client")?;
        Ok((subscriber, admin))
    }
}

impl GcpPubSubSource {
    pub async fn try_new(
        source_runtime: SourceRuntime,
        source_params: PubSubSourceParams,
    ) -> anyhow::Result<Self> {
        let subscription_name = source_params.subscription;
        let backfill_mode_enabled = source_params.enable_backfill_mode;
        let max_messages_per_pull = source_params
            .max_messages_per_pull
            .unwrap_or(DEFAULT_MAX_MESSAGES_PER_PULL);

        let credentials = match source_params.credentials_file {
            Some(credentials_file) => Some(load_credentials(&credentials_file).await?),
            None => None,
        };

        let (subscriber, admin) = build_pubsub_clients(credentials).await?;

        let subscription_fqsn = fully_qualified_subscription_name(
            &subscription_name,
            source_params.project_id.as_deref(),
        );

        admin
            .get_subscription()
            .set_subscription(subscription_fqsn.clone())
            .send()
            .await
            .context(format!(
                "GCP PubSub subscription `{subscription_fqsn}` does not exist or is unreachable"
            ))?;

        let message_stream = subscriber.subscribe(subscription_fqsn.clone()).build();

        // TODO: replace with "<node_id>/<index_id>/<source_id>/<pipeline_ord>"
        let partition_id = append_random_suffix(&format!("gpc-pubsub-{subscription_name}"));
        let partition_id = PartitionId::from(partition_id);

        info!(
            index_id=%source_runtime.index_id(),
            source_id=%source_runtime.source_id(),
            subscription=%subscription_fqsn,
            max_messages_per_pull=%max_messages_per_pull,
            "starting GCP PubSub source"
        );
        Ok(Self {
            source_runtime,
            subscription_name,
            subscription_fqsn,
            message_stream: Mutex::new(message_stream),
            state: GcpPubSubSourceState::default(),
            backfill_mode_enabled,
            partition_id,
            max_messages_per_pull,
        })
    }

    fn should_exit(&self) -> bool {
        self.backfill_mode_enabled && self.state.num_consecutive_empty_batches >= 10
    }
}

#[async_trait]
impl Source for GcpPubSubSource {
    async fn emit_batches(
        &mut self,
        source_sink: &SourceSink,
        ctx: &SourceContext,
    ) -> Result<Duration, ActorExitStatus> {
        let now = Instant::now();
        let mut batch_builder = BatchBuilder::new(SourceType::PubSub);
        let deadline = time::sleep(*EMIT_BATCHES_TIMEOUT);
        tokio::pin!(deadline);
        // TODO: ensure we ACK the message after being commit: at least once
        // TODO: ensure we increase_ack_deadline for the items
        loop {
            tokio::select! {
                resp = self.pull_message_batch(&mut batch_builder) => {
                    if let Err(err) = resp {
                        warn!("failed to pull messages from subscription `{}`: {:?}", self.subscription_fqsn, err);
                    }
                    if batch_builder.num_bytes >= BATCH_NUM_BYTES_LIMIT {
                        break;
                    }
                }
                _ = &mut deadline => {
                    break;
                }
            }
            ctx.record_progress();
        }

        if batch_builder.num_bytes > 0 {
            self.state.num_consecutive_empty_batches = 0
        } else {
            self.state.num_consecutive_empty_batches += 1
        }

        // TODO: need to wait for all the id to be ack for at_least_once
        if self.should_exit() {
            info!(subscription=%self.subscription_fqsn, "reached end of subscription");
            source_sink.send_exit_with_success(ctx).await?;
            return Err(ActorExitStatus::Success);
        }
        if !batch_builder.checkpoint_delta.is_empty() {
            debug!(
                num_bytes=%batch_builder.num_bytes,
                num_docs=%batch_builder.docs.len(),
                num_millis=%now.elapsed().as_millis(),
                "Sending doc batch to indexer.");
            let message = batch_builder.build();
            source_sink.send_raw_doc_batch(message, ctx).await?;
        }
        Ok(Duration::default())
    }

    async fn suggest_truncate(
        &mut self,
        _checkpoint: SourceCheckpoint,
        _ctx: &SourceContext,
    ) -> anyhow::Result<()> {
        // TODO: add ack of ids
        Ok(())
    }

    fn name(&self) -> String {
        format!("{self:?}")
    }

    fn observable_state(&self) -> JsonValue {
        json!({
            "index_id": self.source_runtime.index_id(),
            "source_id": self.source_runtime.source_id(),
            "subscription": self.subscription_name,
            "num_bytes_processed": self.state.num_bytes_processed,
            "num_messages_processed": self.state.num_messages_processed,
            "num_invalid_messages": self.state.num_invalid_messages,
            "num_consecutive_empty_batches": self.state.num_consecutive_empty_batches,
        })
    }
}

impl GcpPubSubSource {
    async fn pull_message_batch(&mut self, batch: &mut BatchBuilder) -> anyhow::Result<()> {
        let mut stream = self.message_stream.lock().await;
        let mut pulled_messages = Vec::new();

        while pulled_messages.len() < self.max_messages_per_pull as usize {
            let next_message = stream.next();
            let Some(result) = next_message.now_or_never().flatten() else {
                break;
            };
            let (message, handler) = result.context("failed to pull messages from subscription")?;
            handler.ack();
            pulled_messages.push(message);
        }

        let Some(last_message) = pulled_messages.last() else {
            return Ok(());
        };
        let message_id = last_message.message_id.clone();
        let publish_timestamp_millis = last_message
            .publish_time
            .as_ref()
            .map(|timestamp| timestamp.seconds() * 1_000 + (timestamp.nanos() as i64 / 1_000_000))
            .unwrap_or(0);

        for message in pulled_messages {
            self.state.num_messages_processed += 1;
            self.state.num_bytes_processed += message.data.len() as u64;
            let doc: Bytes = message.data;
            if doc.is_empty() {
                self.state.num_invalid_messages += 1;
            } else {
                batch.add_doc(doc);
            }
        }
        let to_position = Position::from(format!(
            "{}:{message_id}:{publish_timestamp_millis}",
            self.state.num_messages_processed
        ));
        let from_position = mem::replace(&mut self.state.current_position, to_position.clone());

        batch
            .checkpoint_delta
            .record_partition_delta(self.partition_id.clone(), from_position, to_position)
            .context("failed to record partition delta")?;
        Ok(())
    }
}

// TODO: first implementation of the test
// After we need to ensure at_least_once and concurrent pipeline
#[cfg(all(test, feature = "gcp-pubsub-emulator-tests"))]
mod gcp_pubsub_emulator_tests {
    use std::env::var;
    use std::num::NonZeroUsize;

    use google_cloud_pubsub::client::{Publisher, SubscriptionAdmin, TopicAdmin};
    use google_cloud_pubsub::model::Message;
    use quickwit_actors::Universe;
    use quickwit_config::{SourceConfig, SourceInputFormat, SourceParams};
    use quickwit_proto::types::{IndexId, IndexUid};
    use serde_json::json;

    use super::*;
    use crate::actors::DocProcessor;
    use crate::models::RawDocBatch;
    use crate::source::tests::SourceRuntimeBuilder;
    use crate::source::{SourceActor, quickwit_supported_sources};

    static GCP_TEST_PROJECT: &str = "quickwit-emulator";

    fn get_source_config(subscription: &str) -> SourceConfig {
        var("PUBSUB_EMULATOR_HOST").expect(
            "environment variable `PUBSUB_EMULATOR_HOST` should be set when running GCP PubSub \
             source tests",
        );
        let source_id = append_random_suffix("test-gcp-pubsub-source--source");
        SourceConfig {
            source_id,
            num_pipelines: NonZeroUsize::MIN,
            enabled: true,
            source_params: SourceParams::PubSub(PubSubSourceParams {
                project_id: Some(GCP_TEST_PROJECT.to_string()),
                enable_backfill_mode: true,
                subscription: subscription.to_string(),
                credentials_file: None,
                max_messages_per_pull: None,
            }),
            transform_config: None,
            input_format: SourceInputFormat::Json,
        }
    }

    async fn create_topic_and_subscription(topic: &str, subscription: &str) -> Publisher {
        let topic_admin = TopicAdmin::builder().build().await.unwrap();
        let subscription_admin = SubscriptionAdmin::builder().build().await.unwrap();

        let topic_name = format!("projects/{GCP_TEST_PROJECT}/topics/{topic}");
        let subscription_name = format!("projects/{GCP_TEST_PROJECT}/subscriptions/{subscription}");

        topic_admin
            .create_topic()
            .set_name(topic_name.clone())
            .send()
            .await
            .unwrap();
        subscription_admin
            .create_subscription()
            .set_name(subscription_name)
            .set_topic(topic_name.clone())
            .send()
            .await
            .unwrap();
        Publisher::builder(topic_name).build().await.unwrap()
    }

    #[tokio::test]
    async fn test_gcp_pubsub_source_invalid_subscription() {
        let subscription =
            append_random_suffix("test-gcp-pubsub-source--invalid-subscription--subscription");
        let source_config = get_source_config(&subscription);

        let index_id = append_random_suffix("test-gcp-pubsub-source--invalid-subscription--index");
        let index_uid = IndexUid::new_with_random_ulid(&index_id);
        let SourceParams::PubSub(params) = source_config.clone().source_params else {
            panic!(
                "Expected `SourceParams::GcpPubSub` source params, got {:?}",
                source_config.source_params
            );
        };
        let source_runtime = SourceRuntimeBuilder::new(index_uid, source_config).build();
        GcpPubSubSource::try_new(source_runtime, params)
            .await
            .unwrap_err();
    }

    #[tokio::test]
    async fn test_gcp_pubsub_source() {
        let universe = Universe::with_accelerated_time();

        let topic = append_random_suffix("test-gcp-pubsub-source--topic");
        let subscription = append_random_suffix("test-gcp-pubsub-source--subscription");
        let publisher = create_topic_and_subscription(&topic, &subscription).await;

        let source_config = get_source_config(&subscription);
        let source_id = source_config.source_id.clone();

        let source_loader = quickwit_supported_sources();
        let index_id: IndexId = append_random_suffix("test-gcp-pubsub-source--index");
        let index_uid = IndexUid::new_with_random_ulid(&index_id);

        for i in 0..6 {
            publisher
                .publish(Message::new().set_data(format!("Message {i}")))
                .await
                .unwrap();
        }
        let source_runtime = SourceRuntimeBuilder::new(index_uid, source_config).build();
        let source = source_loader.load_source(source_runtime).await.unwrap();

        let (doc_processor_mailbox, doc_processor_inbox) =
            universe.create_test_mailbox::<DocProcessor>();
        let source_actor = SourceActor::new(source, doc_processor_mailbox);
        let (_source_mailbox, source_handle) = universe.spawn_builder().spawn(source_actor);
        let (exit_status, exit_state) = source_handle.join().await;
        assert!(exit_status.is_success());

        let messages: Vec<RawDocBatch> = doc_processor_inbox.drain_for_test_typed();
        assert_eq!(messages.len(), 1);
        let expected_docs = vec![
            "Message 0",
            "Message 1",
            "Message 2",
            "Message 3",
            "Message 4",
            "Message 5",
        ];
        assert_eq!(messages[0].docs, expected_docs);
        let expected_exit_state = json!({
            "index_id": index_id,
            "source_id": source_id,
            "subscription": subscription,
            "num_bytes_processed": 54,
            "num_messages_processed": 6,
            "num_invalid_messages": 0,
            "num_consecutive_empty_batches": 10,
        });
        assert_eq!(exit_state, expected_exit_state);
    }
}
