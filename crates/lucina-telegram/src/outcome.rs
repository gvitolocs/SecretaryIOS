//! Pure delivery decision, kept independent of transport for a Verus mirror.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendOutcome {
    Delivered,
    NotDelivered,
    Uncertain,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendStage {
    Connect,
    Sent,
}

pub fn classify(stage: SendStage, http_status: Option<u16>, api_ok: Option<bool>) -> SendOutcome {
    if api_ok == Some(true) {
        SendOutcome::Delivered
    } else if api_ok == Some(false)
        || http_status.is_some_and(|s| s >= 400)
        || stage == SendStage::Connect
    {
        SendOutcome::NotDelivered
    } else if http_status.is_none() {
        SendOutcome::Uncertain
    } else {
        // A complete response did not acknowledge delivery.
        SendOutcome::NotDelivered
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    #[test]
    fn rows() {
        for stage in [SendStage::Connect, SendStage::Sent] {
            assert_eq!(
                classify(stage, Some(200), Some(true)),
                SendOutcome::Delivered
            );
            for status in [200, 400, 429, 500] {
                assert_eq!(
                    classify(stage, Some(status), Some(false)),
                    SendOutcome::NotDelivered
                );
            }
            for status in [400, 429, 500] {
                assert_eq!(
                    classify(stage, Some(status), None),
                    SendOutcome::NotDelivered
                );
            }
        }
        assert_eq!(
            classify(SendStage::Connect, None, None),
            SendOutcome::NotDelivered
        );
        assert_eq!(
            classify(SendStage::Sent, None, None),
            SendOutcome::Uncertain
        );
    }
    proptest! {
        #[test]
        fn uncertain_only_after_send(sent in any::<bool>(), status in proptest::option::of(100u16..600), ok in proptest::option::of(any::<bool>())) {
            let stage = if sent { SendStage::Sent } else { SendStage::Connect };
            if classify(stage, status, ok) == SendOutcome::Uncertain {
                prop_assert!(sent && status.is_none());
            }
        }
    }
}
