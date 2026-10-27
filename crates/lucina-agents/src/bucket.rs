//! State buckets for lists and sidebars (Paseo `agent-state-bucket.js`).

use serde::{Deserialize, Serialize};

use crate::types::{AgentStatus, AttentionReason};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Bucket {
    NeedsInput,
    Failed,
    Running,
    Attention,
    Done,
}

impl Bucket {
    /// Paseo `getWorkspaceStateBucketPriority` (0 = most urgent).
    pub fn priority(self) -> u8 {
        match self {
            Bucket::NeedsInput => 0,
            Bucket::Failed => 1,
            Bucket::Running => 2,
            Bucket::Attention => 3,
            Bucket::Done => 4,
        }
    }
}

/// What the bucket rules read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BucketInput {
    pub status: AgentStatus,
    pub pending_permission_count: usize,
    pub requires_attention: bool,
    pub attention_reason: Option<AttentionReason>,
}

/// Paseo `deriveAgentStateBucket`: the first matching rule wins.
pub fn state_bucket(input: BucketInput) -> Bucket {
    if input.pending_permission_count > 0
        || input.attention_reason == Some(AttentionReason::Permission)
    {
        return Bucket::NeedsInput;
    }
    if input.status == AgentStatus::Error || input.attention_reason == Some(AttentionReason::Error) {
        return Bucket::Failed;
    }
    if input.status == AgentStatus::Running {
        return Bucket::Running;
    }
    if input.requires_attention {
        return Bucket::Attention;
    }
    Bucket::Done
}

/// Paseo `getAgentStatusPriority`: like the bucket, but `initializing` ranks 3.
pub fn status_priority(input: BucketInput) -> u8 {
    if input.pending_permission_count > 0
        || input.attention_reason == Some(AttentionReason::Permission)
    {
        return 0;
    }
    if input.status == AgentStatus::Error || input.attention_reason == Some(AttentionReason::Error) {
        return 1;
    }
    match input.status {
        AgentStatus::Running => 2,
        AgentStatus::Initializing => 3,
        _ => 4,
    }
}
