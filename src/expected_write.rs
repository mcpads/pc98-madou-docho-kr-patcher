use anyhow::{Context, Result, ensure};

#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) struct FixedRangeExpectedWrite {
    pub writer: &'static str,
    pub purpose: &'static str,
    pub offset: usize,
    pub expected_source: Vec<u8>,
    pub replacement: Vec<u8>,
}

pub(crate) fn apply_fixed_range_expected_writes(
    source: &[u8],
    writes: &[FixedRangeExpectedWrite],
) -> Result<Vec<u8>> {
    let mut ordered = writes.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|write| write.offset);

    let mut previous_end = 0usize;
    for (index, write) in ordered.iter().enumerate() {
        ensure!(
            write.expected_source.len() == write.replacement.len(),
            "{} expected write for {} changes fixed-range length from {} to {}",
            write.writer,
            write.purpose,
            write.expected_source.len(),
            write.replacement.len()
        );
        let end = write
            .offset
            .checked_add(write.expected_source.len())
            .with_context(|| {
                format!(
                    "{} expected write for {} overflows its source range",
                    write.writer, write.purpose
                )
            })?;
        let actual = source.get(write.offset..end).with_context(|| {
            format!(
                "{} expected write for {} lies outside the immutable source",
                write.writer, write.purpose
            )
        })?;
        ensure!(
            actual == write.expected_source,
            "{} expected write for {} does not match the immutable source",
            write.writer,
            write.purpose
        );
        if index > 0 {
            ensure!(
                previous_end <= write.offset,
                "expected writes overlap between immutable-source offsets 0x{:X} and 0x{:X}",
                write.offset,
                previous_end
            );
        }
        previous_end = end;
    }

    let mut output = source.to_vec();
    for write in ordered {
        let end = write.offset + write.replacement.len();
        output[write.offset..end].copy_from_slice(&write.replacement);
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(
        writer: &'static str,
        offset: usize,
        expected_source: &[u8],
        replacement: &[u8],
    ) -> FixedRangeExpectedWrite {
        FixedRangeExpectedWrite {
            writer,
            purpose: "fixture replacement",
            offset,
            expected_source: expected_source.to_vec(),
            replacement: replacement.to_vec(),
        }
    }

    #[test]
    fn fixed_range_writes_verify_the_immutable_source_before_application() {
        let source = b"ABCDEFGH";
        let writes = [
            write("second", 6, b"GH", b"78"),
            write("first", 1, b"BC", b"23"),
        ];

        let output = apply_fixed_range_expected_writes(source, &writes).unwrap();

        assert_eq!(&output, b"A23DEF78");
        assert_eq!(source, b"ABCDEFGH");
    }

    #[test]
    fn source_mismatch_rejects_the_whole_write_plan() {
        let source = b"ABCDEFGH";
        let writes = [
            write("first", 1, b"BC", b"23"),
            write("mismatch", 5, b"XX", b"67"),
        ];

        let error = apply_fixed_range_expected_writes(source, &writes).unwrap_err();

        assert!(error.to_string().contains("immutable source"));
        assert_eq!(source, b"ABCDEFGH");
    }

    #[test]
    fn overlapping_writers_are_rejected_before_application() {
        let source = b"ABCDEFGH";
        let writes = [
            write("first", 1, b"BCD", b"234"),
            write("second", 3, b"DE", b"45"),
        ];

        let error = apply_fixed_range_expected_writes(source, &writes).unwrap_err();

        assert!(error.to_string().contains("overlap"));
        assert_eq!(source, b"ABCDEFGH");
    }
}
