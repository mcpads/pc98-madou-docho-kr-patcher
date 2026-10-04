use std::collections::BTreeSet;

use anyhow::{Result, bail};

pub(crate) const PARTICLE_MARKERS: [char; 4] = ['\u{E000}', '\u{E001}', '\u{E002}', '\u{E003}'];
pub(crate) const PARTICLE_FORMS: [char; 8] = ['를', '을', '가', '이', '는', '은', '와', '과'];

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum KoreanParticle {
    Object,
    Subject,
    Topic,
    With,
}

impl KoreanParticle {
    pub(crate) const ALL: [Self; 4] = [Self::Object, Self::Subject, Self::Topic, Self::With];

    pub(crate) fn parse(form: &str) -> Option<Self> {
        match form {
            "을" | "를" => Some(Self::Object),
            "이" | "가" => Some(Self::Subject),
            "은" | "는" => Some(Self::Topic),
            "와" | "과" => Some(Self::With),
            _ => None,
        }
    }

    pub(crate) const fn index(self) -> usize {
        match self {
            Self::Object => 0,
            Self::Subject => 1,
            Self::Topic => 2,
            Self::With => 3,
        }
    }

    pub(crate) const fn marker(self) -> char {
        PARTICLE_MARKERS[self.index()]
    }

    pub(crate) const fn forms(self) -> (char, char) {
        match self {
            Self::Object => ('를', '을'),
            Self::Subject => ('가', '이'),
            Self::Topic => ('는', '은'),
            Self::With => ('와', '과'),
        }
    }

    #[cfg(test)]
    fn select_for(self, preceding: char) -> Result<char> {
        let (without_batchim, with_batchim) = self.forms();
        Ok(if has_batchim(preceding)? {
            with_batchim
        } else {
            without_batchim
        })
    }
}

pub(crate) fn has_batchim(character: char) -> Result<bool> {
    if !is_modern_hangul(character) {
        bail!("particle selection requires a precomposed Hangul syllable, got {character:?}");
    }
    Ok(!(character as u32 - '가' as u32).is_multiple_of(28))
}

pub(crate) fn is_modern_hangul(character: char) -> bool {
    ('가'..='힣').contains(&character)
}

pub(crate) fn is_particle_marker(character: char) -> bool {
    PARTICLE_MARKERS.contains(&character)
}

pub(crate) fn plan_renderer_characters(characters: &BTreeSet<char>) -> Result<Vec<char>> {
    if let Some(character) = characters
        .iter()
        .copied()
        .find(|character| !is_modern_hangul(*character))
    {
        bail!("renderer font demand contains a non-Hangul character {character:?}");
    }

    let mut drawable = characters.clone();
    drawable.extend(PARTICLE_FORMS);
    let (without_batchim, with_batchim): (Vec<_>, Vec<_>) = drawable
        .into_iter()
        .partition(|character| !has_batchim(*character).expect("drawable entries are Hangul"));
    Ok(without_batchim
        .into_iter()
        .chain(with_batchim)
        .chain(PARTICLE_MARKERS)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn particles_follow_the_preceding_syllables_batchim() {
        assert_eq!(KoreanParticle::Topic.select_for('피').unwrap(), '는');
        assert_eq!(KoreanParticle::Topic.select_for('클').unwrap(), '은');
        assert_eq!(KoreanParticle::Object.select_for('초').unwrap(), '를');
        assert_eq!(KoreanParticle::Object.select_for('실').unwrap(), '을');
    }

    #[test]
    fn renderer_order_has_one_batchim_boundary_before_the_markers() {
        let characters = ['가', '각', '나', '난'].into_iter().collect();
        let planned = plan_renderer_characters(&characters).unwrap();
        let marker_start = planned
            .iter()
            .position(|character| is_particle_marker(*character))
            .unwrap();
        let drawable = &planned[..marker_start];
        let first_with_batchim = drawable
            .iter()
            .position(|character| has_batchim(*character).unwrap())
            .unwrap();

        assert!(
            drawable[..first_with_batchim]
                .iter()
                .all(|character| !has_batchim(*character).unwrap())
        );
        assert!(
            drawable[first_with_batchim..]
                .iter()
                .all(|character| has_batchim(*character).unwrap())
        );
        assert_eq!(&planned[marker_start..], &PARTICLE_MARKERS);
    }
}
