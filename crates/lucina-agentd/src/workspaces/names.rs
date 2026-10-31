//! Random adjective-noun names for worktrees made without a name or a prompt (Paseo
//! uses the `mnemonic-id` package for the same, e.g. `empty-crocodile`).

const ADJECTIVES: &[&str] = &[
    "amber", "bold", "brave", "bright", "calm", "clever", "cosmic", "crisp", "daring", "eager", "early", "fancy",
    "gentle", "glad", "golden", "grand", "happy", "honest", "jolly", "keen", "kind", "lively", "lucky", "merry",
    "mighty", "misty", "nimble", "noble", "plain", "polite", "proud", "quick", "quiet", "rapid", "rare", "ready",
    "rosy", "royal", "rustic", "shy", "silent", "silver", "simple", "sleek", "smooth", "snowy", "solid", "spry",
    "steady", "still", "sunny", "swift", "tidy", "tiny", "vivid", "warm", "wild", "wise", "witty", "young",
];

const NOUNS: &[&str] = &[
    "badger", "beacon", "bison", "breeze", "brook", "canyon", "cedar", "comet", "coral", "crane", "delta", "eagle",
    "ember", "falcon", "fern", "finch", "fjord", "fox", "gecko", "glacier", "harbor", "hawk", "heron", "island",
    "lagoon", "lark", "lemur", "lynx", "maple", "meadow", "meteor", "moose", "nebula", "oak", "orca", "otter",
    "owl", "panda", "pebble", "pine", "planet", "quartz", "raven", "reef", "river", "robin", "sparrow", "spruce",
    "summit", "swan", "thicket", "tiger", "trail", "tundra", "valley", "walrus", "willow", "wolf", "wren", "yak",
];

fn pick(list: &[&'static str], seed: u128) -> &'static str {
    list[(seed % list.len() as u128) as usize]
}

pub fn random() -> String {
    let n = uuid::Uuid::new_v4().as_u128();
    format!("{}-{}", pick(ADJECTIVES, n), pick(NOUNS, n >> 64))
}
