use arc_bytes::OwnedBytes;
use async_trait::async_trait;
use circulate::{flume, Message, Relay};
use serde::Serialize;

use crate::Error;

/// Publishes and Subscribes to messages on topics.
pub trait PubSub {
    /// The Subscriber type for this `PubSub` connection.
    type Subscriber: Subscriber;

    /// Create a new [`Subscriber`] for this relay.
    fn create_subscriber(&self) -> Result<Self::Subscriber, Error>;

    /// Publishes a `payload` to all subscribers of `topic`.
    fn publish<Topic: Serialize, Payload: Serialize>(
        &self,
        topic: &Topic,
        payload: &Payload,
    ) -> Result<(), Error> {
        self.publish_bytes(pot::to_vec(topic)?, pot::to_vec(payload)?)
    }

    /// Publishes a `payload` to all subscribers of `topic`.
    fn publish_bytes(&self, topic: Vec<u8>, payload: Vec<u8>) -> Result<(), Error>;

    /// Publishes a `payload` to all subscribers of all `topics`.
    fn publish_to_all<
        'topics,
        Topics: IntoIterator<Item = &'topics Topic> + 'topics,
        Topic: Serialize + 'topics,
        Payload: Serialize,
    >(
        &self,
        topics: Topics,
        payload: &Payload,
    ) -> Result<(), Error> {
        let topics = topics
            .into_iter()
            .map(pot::to_vec)
            .collect::<Result<Vec<_>, _>>()?;
        self.publish_bytes_to_all(topics, pot::to_vec(payload)?)
    }

    /// Publishes a `payload` to all subscribers of all `topics`.
    fn publish_bytes_to_all(
        &self,
        topics: impl IntoIterator<Item = Vec<u8>> + Send,
        payload: Vec<u8>,
    ) -> Result<(), Error>;
}

/// A subscriber to one or more topics.
pub trait Subscriber {
    /// Subscribe to [`Message`]s published to `topic`.
    fn subscribe_to<Topic: Serialize>(&self, topic: &Topic) -> Result<(), Error> {
        self.subscribe_to_bytes(pot::to_vec(topic)?)
    }

    /// Subscribe to [`Message`]s published to `topic`.
    fn subscribe_to_bytes(&self, topic: Vec<u8>) -> Result<(), Error>;

    /// Unsubscribe from [`Message`]s published to `topic`.
    fn unsubscribe_from<Topic: Serialize>(&self, topic: &Topic) -> Result<(), Error> {
        self.unsubscribe_from_bytes(&pot::to_vec(topic)?)
    }

    /// Unsubscribe from [`Message`]s published to `topic`.
    fn unsubscribe_from_bytes(&self, topic: &[u8]) -> Result<(), Error>;

    /// Returns the receiver to receive [`Message`]s.
    #[must_use]
    fn receiver(&self) -> &'_ flume::Receiver<Message>;
}

/// Publishes and Subscribes to messages on topics.
#[async_trait]
pub trait AsyncPubSub: Send + Sync {
    /// The Subscriber type for this `PubSub` connection.
    type Subscriber: AsyncSubscriber;

    /// Create a new [`Subscriber`] for this relay.
    async fn create_subscriber(&self) -> Result<Self::Subscriber, Error>;

    /// Publishes a `payload` to all subscribers of `topic`.
    async fn publish<Topic: Serialize + Send + Sync, Payload: Serialize + Send + Sync>(
        &self,
        topic: &Topic,
        payload: &Payload,
    ) -> Result<(), Error> {
        let topic = pot::to_vec(topic)?;
        let payload = pot::to_vec(payload)?;
        self.publish_bytes(topic, payload).await
    }

    /// Publishes a `payload` to all subscribers of `topic`.
    async fn publish_bytes(&self, topic: Vec<u8>, payload: Vec<u8>) -> Result<(), Error>;

    /// Publishes a `payload` to all subscribers of all `topics`.
    async fn publish_to_all<
        'topics,
        Topics: IntoIterator<Item = &'topics Topic> + Send + 'topics,
        Topic: Serialize + Send + 'topics,
        Payload: Serialize + Send + Sync,
    >(
        &self,
        topics: Topics,
        payload: &Payload,
    ) -> Result<(), Error> {
        let topics = topics
            .into_iter()
            .map(|topic| pot::to_vec(topic))
            .collect::<Result<Vec<_>, _>>()?;
        self.publish_bytes_to_all(topics, pot::to_vec(payload)?)
            .await
    }

    /// Publishes a `payload` to all subscribers of all `topics`.
    async fn publish_bytes_to_all(
        &self,
        topics: impl IntoIterator<Item = Vec<u8>> + Send + 'async_trait,
        payload: Vec<u8>,
    ) -> Result<(), Error>;
}

/// A subscriber to one or more topics.
#[async_trait]
pub trait AsyncSubscriber: Send + Sync {
    /// Subscribe to [`Message`]s published to `topic`.
    async fn subscribe_to<Topic: Serialize + Send + Sync>(
        &self,
        topic: &Topic,
    ) -> Result<(), Error> {
        self.subscribe_to_bytes(pot::to_vec(topic)?).await
    }

    /// Subscribe to [`Message`]s published to `topic`.
    async fn subscribe_to_bytes(&self, topic: Vec<u8>) -> Result<(), Error>;

    /// Unsubscribe from [`Message`]s published to `topic`.
    async fn unsubscribe_from<Topic: Serialize + Send + Sync>(
        &self,
        topic: &Topic,
    ) -> Result<(), Error> {
        self.unsubscribe_from_bytes(&pot::to_vec(topic)?).await
    }

    /// Unsubscribe from [`Message`]s published to `topic`.
    async fn unsubscribe_from_bytes(&self, topic: &[u8]) -> Result<(), Error>;

    /// Returns the receiver to receive [`Message`]s.
    #[must_use]
    fn receiver(&self) -> &'_ flume::Receiver<Message>;
}

#[async_trait]
impl PubSub for Relay {
    type Subscriber = circulate::Subscriber;

    fn create_subscriber(&self) -> Result<Self::Subscriber, Error> {
        Ok(self.create_subscriber())
    }

    fn publish<Topic: Serialize, Payload: Serialize>(
        &self,
        topic: &Topic,
        payload: &Payload,
    ) -> Result<(), Error> {
        self.publish(topic, payload)?;
        Ok(())
    }

    fn publish_to_all<
        'topics,
        Topics: IntoIterator<Item = &'topics Topic> + 'topics,
        Topic: Serialize + 'topics,
        Payload: Serialize,
    >(
        &self,
        topics: Topics,
        payload: &Payload,
    ) -> Result<(), Error> {
        self.publish_to_all(topics, payload)?;
        Ok(())
    }

    fn publish_bytes(&self, topic: Vec<u8>, payload: Vec<u8>) -> Result<(), Error> {
        self.publish_raw(topic, payload);
        Ok(())
    }

    fn publish_bytes_to_all(
        &self,
        topics: impl IntoIterator<Item = Vec<u8>>,
        payload: Vec<u8>,
    ) -> Result<(), Error> {
        self.publish_raw_to_all(topics.into_iter().map(OwnedBytes::from), payload);
        Ok(())
    }
}

#[async_trait]
impl AsyncPubSub for Relay {
    type Subscriber = circulate::Subscriber;

    async fn create_subscriber(&self) -> Result<Self::Subscriber, Error> {
        Ok(self.create_subscriber())
    }

    /// Publishes a `payload` to all subscribers of `topic`.
    async fn publish_bytes(&self, topic: Vec<u8>, payload: Vec<u8>) -> Result<(), Error> {
        self.publish_raw(topic, payload);
        Ok(())
    }

    async fn publish_bytes_to_all(
        &self,
        topics: impl IntoIterator<Item = Vec<u8>> + Send + 'async_trait,
        payload: Vec<u8>,
    ) -> Result<(), Error> {
        self.publish_raw_to_all(topics.into_iter().map(OwnedBytes::from), payload);
        Ok(())
    }
}

impl Subscriber for circulate::Subscriber {
    fn subscribe_to_bytes(&self, topic: Vec<u8>) -> Result<(), Error> {
        self.subscribe_to_raw(topic);
        Ok(())
    }

    fn unsubscribe_from_bytes(&self, topic: &[u8]) -> Result<(), Error> {
        self.unsubscribe_from_raw(topic);
        Ok(())
    }

    fn receiver(&self) -> &'_ flume::Receiver<Message> {
        self.receiver()
    }
}

#[async_trait]
impl AsyncSubscriber for circulate::Subscriber {
    async fn subscribe_to_bytes(&self, topic: Vec<u8>) -> Result<(), Error> {
        self.subscribe_to_raw(topic);
        Ok(())
    }

    async fn unsubscribe_from_bytes(&self, topic: &[u8]) -> Result<(), Error> {
        self.unsubscribe_from_raw(topic);
        Ok(())
    }

    fn receiver(&self) -> &'_ flume::Receiver<Message> {
        self.receiver()
    }
}

/// Creates a topic for use in a server. This is an internal API, which is why
/// the documentation is hidden. This is an implementation detail, but both
/// Client and Server must agree on this format, which is why it lives in core.
#[doc(hidden)]
#[must_use]
pub fn database_topic(database: &str, topic: &[u8]) -> Vec<u8> {
    let mut namespaced_topic = Vec::with_capacity(database.len() + topic.len() + 1);

    namespaced_topic.extend(database.bytes());
    namespaced_topic.push(b'\0');
    namespaced_topic.extend(topic);

    namespaced_topic
}

/// Expands into a suite of pubsub unit tests using the passed type as the test harness.
#[cfg(any(test, feature = "test-util"))]
#[cfg_attr(feature = "test-util", macro_export)]
macro_rules! define_pubsub_test_suite {
    ($harness:ident) => {
        #[cfg(test)]
        use $crate::pubsub::{AsyncPubSub, AsyncSubscriber};

        #[tokio::test]
        async fn simple_pubsub_test() -> anyhow::Result<()> {
            let harness = $harness::new($crate::test_util::HarnessTest::PubSubSimple).await?;
            let pubsub = harness.connect().await?;
            let subscriber = AsyncPubSub::create_subscriber(&pubsub).await?;
            AsyncSubscriber::subscribe_to(&subscriber, &"mytopic").await?;
            AsyncPubSub::publish(&pubsub, &"mytopic", &String::from("test")).await?;
            AsyncPubSub::publish(&pubsub, &"othertopic", &String::from("test")).await?;
            let receiver = subscriber.receiver().clone();
            let message = receiver.recv_async().await.expect("No message received");
            assert_eq!(message.payload::<String>()?, "test");
            // The message should only be received once.
            assert!(matches!(
                tokio::task::spawn_blocking(
                    move || receiver.recv_timeout(std::time::Duration::from_millis(100))
                )
                .await,
                Ok(Err(_))
            ));
            Ok(())
        }

        #[tokio::test]
        async fn multiple_subscribers_test() -> anyhow::Result<()> {
            let harness =
                $harness::new($crate::test_util::HarnessTest::PubSubMultipleSubscribers).await?;
            let pubsub = harness.connect().await?;
            let subscriber_a = AsyncPubSub::create_subscriber(&pubsub).await?;
            let subscriber_ab = AsyncPubSub::create_subscriber(&pubsub).await?;
            AsyncSubscriber::subscribe_to(&subscriber_a, &"a").await?;
            AsyncSubscriber::subscribe_to(&subscriber_ab, &"a").await?;
            AsyncSubscriber::subscribe_to(&subscriber_ab, &"b").await?;

            let mut messages_a = Vec::new();
            let mut messages_ab = Vec::new();
            AsyncPubSub::publish(&pubsub, &"a", &String::from("a1")).await?;
            messages_a.push(
                subscriber_a
                    .receiver()
                    .recv_async()
                    .await?
                    .payload::<String>()?,
            );
            messages_ab.push(
                subscriber_ab
                    .receiver()
                    .recv_async()
                    .await?
                    .payload::<String>()?,
            );

            AsyncPubSub::publish(&pubsub, &"b", &String::from("b1")).await?;
            messages_ab.push(
                subscriber_ab
                    .receiver()
                    .recv_async()
                    .await?
                    .payload::<String>()?,
            );

            AsyncPubSub::publish(&pubsub, &"a", &String::from("a2")).await?;
            messages_a.push(
                subscriber_a
                    .receiver()
                    .recv_async()
                    .await?
                    .payload::<String>()?,
            );
            messages_ab.push(
                subscriber_ab
                    .receiver()
                    .recv_async()
                    .await?
                    .payload::<String>()?,
            );

            assert_eq!(&messages_a[0], "a1");
            assert_eq!(&messages_a[1], "a2");

            assert_eq!(&messages_ab[0], "a1");
            assert_eq!(&messages_ab[1], "b1");
            assert_eq!(&messages_ab[2], "a2");

            Ok(())
        }

        #[tokio::test]
        async fn unsubscribe_test() -> anyhow::Result<()> {
            let harness = $harness::new($crate::test_util::HarnessTest::PubSubUnsubscribe).await?;
            let pubsub = harness.connect().await?;
            let subscriber = AsyncPubSub::create_subscriber(&pubsub).await?;
            AsyncSubscriber::subscribe_to(&subscriber, &"a").await?;

            AsyncPubSub::publish(&pubsub, &"a", &String::from("a1")).await?;
            AsyncSubscriber::unsubscribe_from(&subscriber, &"a").await?;
            AsyncPubSub::publish(&pubsub, &"a", &String::from("a2")).await?;
            AsyncSubscriber::subscribe_to(&subscriber, &"a").await?;
            AsyncPubSub::publish(&pubsub, &"a", &String::from("a3")).await?;

            // Check subscriber_a for a1 and a2.
            let message = subscriber.receiver().recv_async().await?;
            assert_eq!(message.payload::<String>()?, "a1");
            let message = subscriber.receiver().recv_async().await?;
            assert_eq!(message.payload::<String>()?, "a3");

            Ok(())
        }

        #[tokio::test]
        async fn publish_to_all_test() -> anyhow::Result<()> {
            let harness = $harness::new($crate::test_util::HarnessTest::PubSubPublishAll).await?;
            let pubsub = harness.connect().await?;
            let subscriber_a = AsyncPubSub::create_subscriber(&pubsub).await?;
            let subscriber_b = AsyncPubSub::create_subscriber(&pubsub).await?;
            let subscriber_c = AsyncPubSub::create_subscriber(&pubsub).await?;
            AsyncSubscriber::subscribe_to(&subscriber_a, &"1").await?;
            AsyncSubscriber::subscribe_to(&subscriber_b, &"1").await?;
            AsyncSubscriber::subscribe_to(&subscriber_b, &"2").await?;
            AsyncSubscriber::subscribe_to(&subscriber_c, &"2").await?;
            AsyncSubscriber::subscribe_to(&subscriber_a, &"3").await?;
            AsyncSubscriber::subscribe_to(&subscriber_c, &"3").await?;

            AsyncPubSub::publish_to_all(&pubsub, [&"1", &"2", &"3"], &String::from("1")).await?;

            // Each subscriber should get "1" twice on separate topics
            for subscriber in &[subscriber_a, subscriber_b, subscriber_c] {
                let mut message_topics = Vec::new();
                for _ in 0..2_u8 {
                    let message = subscriber.receiver().recv_async().await?;
                    assert_eq!(message.payload::<String>()?, "1");
                    message_topics.push(message.topic.clone());
                }
                assert!(matches!(
                    subscriber.receiver().try_recv(),
                    Err(flume::TryRecvError::Empty)
                ));
                assert!(message_topics[0] != message_topics[1]);
            }

            Ok(())
        }
    };
}

#[cfg(test)]
mod tests {
    use circulate::{flume, Relay};

    use crate::{test_util::HarnessTest, Error};

    struct Harness {
        relay: Relay,
    }

    impl Harness {
        async fn new(_: HarnessTest) -> Result<Self, Error> {
            Ok(Self {
                relay: Relay::default(),
            })
        }

        async fn connect(&self) -> Result<Relay, Error> {
            Ok(self.relay.clone())
        }
    }

    define_pubsub_test_suite!(Harness);
}
