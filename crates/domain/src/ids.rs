use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};
use utoipa::ToSchema;
use uuid::Uuid;

macro_rules! uuid_id {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(
            Clone,
            Copy,
            Debug,
            Deserialize,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
            Serialize,
            ToSchema,
        )]
        #[serde(transparent)]
        #[schema(value_type = String, format = Uuid)]
        pub struct $name(Uuid);

        impl $name {
            pub const fn from_uuid(value: Uuid) -> Self {
                Self(value)
            }

            pub fn new() -> Self {
                Self(Uuid::now_v7())
            }

            pub const fn as_uuid(self) -> Uuid {
                self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }

        impl From<Uuid> for $name {
            fn from(value: Uuid) -> Self {
                Self(value)
            }
        }

        impl From<$name> for Uuid {
            fn from(value: $name) -> Self {
                value.0
            }
        }

        impl FromStr for $name {
            type Err = uuid::Error;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Uuid::parse_str(value).map(Self)
            }
        }
    };
}

uuid_id!(
    ProviderId,
    "Stable identity of an upstream provider/account."
);
uuid_id!(SourceId, "Stable identity of a provider stream or route.");
uuid_id!(
    ChannelId,
    "Canonical identity of a logical channel, independent of all delivery attributes."
);
uuid_id!(EpgSourceId, "Stable identity of an EPG source.");
uuid_id!(
    EventRuleId,
    "Stable identity of a dynamic event-channel rule."
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_round_trip_as_transparent_strings() {
        let id = ChannelId::new();
        let json = serde_json::to_string(&id).expect("serialize id");
        let decoded: ChannelId = serde_json::from_str(&json).expect("deserialize id");
        assert_eq!(decoded, id);
        assert_eq!(id.to_string().parse::<ChannelId>().expect("parse id"), id);
    }

    #[test]
    fn generated_ids_are_uuid_v7() {
        assert_eq!(ChannelId::new().as_uuid().get_version_num(), 7);
    }

    #[test]
    fn uuid_conversion_traits_preserve_exact_identity() {
        let uuid = Uuid::now_v7();
        let from_constructor = ProviderId::from_uuid(uuid);
        let from_trait = ProviderId::from(uuid);
        assert_eq!(from_constructor, from_trait);
        let round_trip: Uuid = from_trait.into();
        assert_eq!(round_trip, uuid);
    }
}
