//! Identity & Tenancy bounded context: schools, accounts, memberships, duties,
//! classes and parent links. Open host service for every other context.

pub mod domain {
    use kernel::{Duty, Role};
    use rand::seq::SliceRandom;
    use rand::Rng;
    use serde::{Deserialize, Serialize};
    use std::collections::HashSet;

    /// Words for pupil passphrases: short, concrete, easy to read and spell.
    const WORDS: &[&str] = &[
        "maple", "river", "seven", "otter", "cloud", "lemon", "tiger", "brick", "pearl", "comet", "honey", "ocean",
        "panda", "robin", "sunny", "train", "wagon", "zebra", "apple", "beach", "candle", "daisy", "eagle", "forest",
        "garden", "hammer", "island", "jelly", "kettle", "ladder", "meadow", "noodle", "orange", "pepper", "rocket",
        "silver", "tulip", "violet", "window", "yellow", "acorn", "badger", "carrot", "dragon", "feather", "gecko",
        "harbour", "igloo", "jigsaw", "kitten", "lantern", "marble", "nutmeg", "pebble", "puffin", "quartz", "saddle",
        "thimble", "walnut", "button",
    ];

    pub fn passphrase() -> String {
        let mut rng = rand::thread_rng();
        let w: Vec<&str> = WORDS.choose_multiple(&mut rng, 3).copied().collect();
        w.join("-")
    }

    /// 8-character invite code without look-alike characters.
    pub fn invite_code() -> String {
        const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
        let mut rng = rand::thread_rng();
        (0..8).map(|_| ALPHABET[rng.gen_range(0..ALPHABET.len())] as char).collect()
    }

    /// 6-digit one-time code for parent sign-in.
    pub fn one_time_code() -> String {
        format!("{:06}", rand::thread_rng().gen_range(0..1_000_000))
    }

    fn slug(s: &str) -> String {
        s.chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .collect::<String>()
            .to_lowercase()
    }

    /// `firstname.initial`, then `firstname.initial2`, `...3` on clashes.
    pub fn username(first: &str, last: &str, taken: &HashSet<String>) -> String {
        let f = slug(first);
        let l: String = slug(last).chars().take(1).collect();
        let base = if l.is_empty() { f.clone() } else { format!("{f}.{l}") };
        if !taken.contains(&base) {
            return base;
        }
        (2..1000)
            .map(|n| format!("{base}{n}"))
            .find(|u| !taken.contains(u))
            .unwrap_or_else(|| format!("{base}{}", rand::thread_rng().gen_range(1000..9999)))
    }

    /// School codes: 3 to 12 letters or digits, upper case.
    pub fn normalise_school_code(s: &str) -> Option<String> {
        let c: String = s.trim().to_uppercase().chars().filter(|c| !c.is_whitespace()).collect();
        (c.len() >= 3 && c.len() <= 12 && c.chars().all(|c| c.is_ascii_alphanumeric())).then_some(c)
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct ImportRow {
        pub line: usize,
        pub first_name: String,
        pub last_name: String,
        pub year_group: Option<i32>,
        pub class_name: String,
        pub username: String,
        pub problem: Option<String>,
        pub username_changed: bool,
    }

    /// Check a class list before accounts are created. Rows with problems are
    /// reported, never silently fixed, except username clashes, which get a
    /// number and are labelled.
    pub fn check_import(rows: &[(String, String, String, String)], existing: &HashSet<String>) -> Vec<ImportRow> {
        let mut taken = existing.clone();
        rows.iter()
            .enumerate()
            .map(|(i, (first, last, year, class))| {
                let first = first.trim().to_string();
                let last = last.trim().to_string();
                let mut problem = None;
                if first.is_empty() {
                    problem = Some("First name missing".to_string());
                }
                let year_group = year.trim().trim_start_matches(['Y', 'y']).parse::<i32>().ok().filter(|y| (1..=13).contains(y));
                if year_group.is_none() && problem.is_none() {
                    problem = Some("Year group missing or not 1 to 13".to_string());
                }
                let base_taken = taken.contains(&username(&first, &last, &HashSet::new()));
                let u = username(&first, &last, &taken);
                taken.insert(u.clone());
                ImportRow {
                    line: i + 2,
                    first_name: first,
                    last_name: last,
                    year_group,
                    class_name: class.trim().to_string(),
                    username: u,
                    problem,
                    username_changed: base_taken,
                }
            })
            .collect()
    }

    /// A school can let pupils sign in only once both safeguarding duties are held.
    pub fn can_activate(duties_held: &[Duty]) -> Result<(), &'static str> {
        let lead = duties_held.contains(&Duty::SafeguardingLead);
        let deputy = duties_held.contains(&Duty::DeputySafeguardingLead);
        match (lead, deputy) {
            (true, true) => Ok(()),
            (false, _) => Err("Name a Designated Safeguarding Lead before pupils can sign in."),
            (true, false) => Err("Name a deputy safeguarding lead before pupils can sign in."),
        }
    }

    /// Duties may only be held by teachers and school admins.
    pub fn can_hold_duty(role: Role) -> bool {
        matches!(role, Role::Teacher | Role::SchoolAdmin)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn usernames_get_numbers_on_clash() {
            let mut taken = HashSet::new();
            assert_eq!(username("Sam", "Taylor", &taken), "sam.t");
            taken.insert("sam.t".into());
            assert_eq!(username("Sam", "Thomas", &taken), "sam.t2");
        }

        #[test]
        fn import_flags_missing_year_and_clash() {
            let rows = vec![
                ("Sam".into(), "Taylor".into(), "6".into(), "6B".into()),
                ("Sam".into(), "Thomas".into(), "6".into(), "6B".into()),
                ("Mia".into(), "Rossi".into(), "".into(), "6B".into()),
            ];
            let out = check_import(&rows, &HashSet::new());
            assert_eq!(out[1].username, "sam.t2");
            assert!(out[1].username_changed);
            assert!(out[2].problem.is_some());
            assert!(out[0].problem.is_none());
        }

        #[test]
        fn activation_needs_both_duties() {
            assert!(can_activate(&[Duty::SafeguardingLead]).is_err());
            assert!(can_activate(&[Duty::SafeguardingLead, Duty::DeputySafeguardingLead]).is_ok());
        }

        #[test]
        fn passphrase_has_three_words() {
            assert_eq!(passphrase().split('-').count(), 3);
            assert_eq!(invite_code().len(), 8);
            assert_eq!(one_time_code().len(), 6);
        }

        #[test]
        fn school_code_rules() {
            assert_eq!(normalise_school_code(" oakhill "), Some("OAKHILL".into()));
            assert_eq!(normalise_school_code("ab"), None);
        }
    }
}
