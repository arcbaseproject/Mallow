use super::vault::{Result, Secret};
use rand::{Rng, rngs::OsRng};
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(default)]
pub struct Preferences {
    pub save: bool,
    pub fill: bool,
    pub lock_minutes: u32,
    pub length: u32,
    pub upper: bool,
    pub lower: bool,
    pub digits: bool,
    pub symbols: bool,
    pub avoid_ambiguous: bool,
    pub excluded: Vec<String>,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            save: true,
            fill: true,
            lock_minutes: 5,
            length: 24,
            upper: true,
            lower: true,
            digits: true,
            symbols: true,
            avoid_ambiguous: true,
            excluded: vec![],
        }
    }
}
pub fn generate(p: &Preferences) -> Result<Secret> {
    if !(12..=128).contains(&p.length) {
        return Err("Choose a length between 12 and 128.");
    }
    let groups: Vec<Vec<u8>> = [
        (p.upper, "ABCDEFGHIJKLMNOPQRSTUVWXYZ"),
        (p.lower, "abcdefghijklmnopqrstuvwxyz"),
        (p.digits, "0123456789"),
        (p.symbols, "!#$%&()*+,-./:;<=>?@[]^_{|}~"),
    ]
    .into_iter()
    .filter(|(on, _)| *on)
    .map(|(_, s)| {
        s.bytes()
            .filter(|b| !p.avoid_ambiguous || !b"0O1Ilo|".contains(b))
            .collect()
    })
    .collect();
    let alphabet: Vec<_> = groups.iter().flatten().copied().collect();
    if alphabet.is_empty() {
        return Err("Select at least one character group.");
    }
    // Rejection sampling keeps all accepted passwords equally likely.
    loop {
        let mut out = Secret::default();
        for _ in 0..p.length {
            out.push(alphabet[OsRng.gen_range(0..alphabet.len())] as char);
        }
        if groups.iter().all(|g| out.bytes().any(|b| g.contains(&b))) {
            return Ok(out);
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn generator_settings() {
        let p = Preferences::default();
        let a = generate(&p).unwrap();
        assert_eq!(a.len(), 24);
        assert!(a.as_str() != generate(&p).unwrap().as_str());
        assert!(!a.bytes().any(|b| b"0O1Ilo|".contains(&b)));
        assert!(
            generate(&Preferences {
                upper: false,
                lower: false,
                symbols: false,
                digits: false,
                ..p
            })
            .is_err()
        );
    }
}
