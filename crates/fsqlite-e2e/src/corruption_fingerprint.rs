//! bd-2bjaf: Corruption fingerprint extractor and normalizer.
//!
//! Parses SQLite `PRAGMA integrity_check` output into structured failures,
//! then normalizes into stable signatures for grouping identical corruption
//! modes across different databases and runs.

use std::collections::HashMap;
use std::fmt::{self, Write as _};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum FailureKind {
    PageDoubleReference {
        ref_count: u32,
    },
    RowidOutOfOrder,
    FreeSpaceCorruption,
    ExtendsOffEnd,
    OffsetOutOfRange {
        offset: u32,
        range_lo: u32,
        range_hi: u32,
    },
    ChildPageDepthDiffers,
    MultipleUsesForByte,
    FreelistLeafCountTooBig,
    PtrmapReadFailed,
    PageReadFailed,
    PageNeverUsed,
    PagePointerMapReferenced,
    InvalidPageNumber,
    Other(String),
}

impl fmt::Display for FailureKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PageDoubleReference { ref_count } => {
                write!(f, "page_double_ref(count={ref_count})")
            }
            Self::RowidOutOfOrder => write!(f, "rowid_out_of_order"),
            Self::FreeSpaceCorruption => write!(f, "free_space_corruption"),
            Self::ExtendsOffEnd => write!(f, "extends_off_end"),
            Self::OffsetOutOfRange {
                offset,
                range_lo,
                range_hi,
            } => {
                write!(f, "offset_out_of_range({offset},{range_lo}..{range_hi})")
            }
            Self::ChildPageDepthDiffers => write!(f, "child_page_depth_differs"),
            Self::MultipleUsesForByte => write!(f, "multiple_uses_for_byte"),
            Self::FreelistLeafCountTooBig => write!(f, "freelist_leaf_count_too_big"),
            Self::PtrmapReadFailed => write!(f, "ptrmap_read_failed"),
            Self::PageReadFailed => write!(f, "page_read_failed"),
            Self::PageNeverUsed => write!(f, "page_never_used"),
            Self::PagePointerMapReferenced => write!(f, "page_pointer_map_referenced"),
            Self::InvalidPageNumber => write!(f, "invalid_page_number"),
            Self::Other(s) => write!(f, "other({s})"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub tree_id: Option<u32>,
    pub page: Option<u32>,
    pub cell: Option<u32>,
    pub kind: FailureKind,
    pub referenced_page: Option<u32>,
    pub raw_line: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TreeIdClass {
    Small,
    Medium,
    Large,
}

impl TreeIdClass {
    #[must_use]
    pub fn from_id(id: u32) -> Self {
        match id {
            0..=10 => Self::Small,
            11..=100 => Self::Medium,
            _ => Self::Large,
        }
    }
}

impl fmt::Display for TreeIdClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Small => write!(f, "small"),
            Self::Medium => write!(f, "medium"),
            Self::Large => write!(f, "large"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RefCountClass {
    Single,
    Few,
    Many,
}

impl RefCountClass {
    #[must_use]
    pub fn from_count(n: u32) -> Self {
        match n {
            0..=1 => Self::Single,
            2..=5 => Self::Few,
            _ => Self::Many,
        }
    }
}

impl fmt::Display for RefCountClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Single => write!(f, "single"),
            Self::Few => write!(f, "few"),
            Self::Many => write!(f, "many"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NormalizedSignature {
    pub tree_id_class: Option<TreeIdClass>,
    pub kind: FailureKind,
    pub ref_count_class: Option<RefCountClass>,
    pub has_cell: bool,
}

impl fmt::Display for NormalizedSignature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(ref tc) = self.tree_id_class {
            write!(f, "tree:{tc}/")?;
        }
        write!(f, "{}", self.kind)?;
        if self.has_cell {
            write!(f, "/cell")?;
        }
        if let Some(ref rc) = self.ref_count_class {
            write!(f, "/refs:{rc}")?;
        }
        Ok(())
    }
}

#[derive(Debug)]
pub struct ParseError {
    pub line_number: usize,
    pub message: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: {}", self.line_number, self.message)
    }
}

impl std::error::Error for ParseError {}

#[must_use]
pub fn parse_failure(line: &str) -> Option<Failure> {
    let trimmed = line.trim();

    if trimmed.is_empty() || trimmed == "ok" || trimmed.starts_with("***") {
        return None;
    }

    if let Some(f) = try_parse_tree_page_cell(trimmed) {
        return Some(f);
    }
    if let Some(f) = try_parse_tree_page(trimmed) {
        return Some(f);
    }
    if let Some(f) = try_parse_freelist(trimmed) {
        return Some(f);
    }
    if let Some(f) = try_parse_page_level(trimmed) {
        return Some(f);
    }
    if let Some(f) = try_parse_bare(trimmed) {
        return Some(f);
    }

    Some(Failure {
        tree_id: None,
        page: None,
        cell: None,
        kind: FailureKind::Other(trimmed.to_owned()),
        referenced_page: None,
        raw_line: trimmed.to_owned(),
    })
}

fn try_parse_tree_page_cell(line: &str) -> Option<Failure> {
    // "Tree N page P cell C: <message>"
    let rest = line.strip_prefix("Tree ")?;
    let (tree_str, rest) = rest.split_once(' ')?;
    let tree_id: u32 = tree_str.parse().ok()?;
    let rest = rest.strip_prefix("page ")?;
    let (page_str, rest) = rest.split_once(' ')?;
    let page: u32 = page_str.parse().ok()?;
    let rest = rest.strip_prefix("cell ")?;
    let (cell_str, rest) = rest.split_once(':')?;
    let cell: u32 = cell_str.parse().ok()?;
    let msg = rest.trim();

    let (kind, ref_page) = parse_message(msg);

    Some(Failure {
        tree_id: Some(tree_id),
        page: Some(page),
        cell: Some(cell),
        kind,
        referenced_page: ref_page,
        raw_line: line.to_owned(),
    })
}

fn try_parse_tree_page(line: &str) -> Option<Failure> {
    // "Tree N page P: <message>" or "Tree N page P right child: <message>"
    let rest = line.strip_prefix("Tree ")?;
    let (tree_str, rest) = rest.split_once(' ')?;
    let tree_id: u32 = tree_str.parse().ok()?;
    let rest = rest.strip_prefix("page ")?;

    let (page_str, rest) = if let Some((p, r)) = rest.split_once(':') {
        let p = p.trim().trim_end_matches(" right child");
        (p, r)
    } else {
        return None;
    };

    let page: u32 = page_str.parse().ok()?;
    let msg = rest.trim();
    let (kind, ref_page) = parse_message(msg);

    Some(Failure {
        tree_id: Some(tree_id),
        page: Some(page),
        cell: None,
        kind,
        referenced_page: ref_page,
        raw_line: line.to_owned(),
    })
}

fn try_parse_freelist(line: &str) -> Option<Failure> {
    let rest = line.strip_prefix("Freelist: ")?;
    let (kind, ref_page) = parse_message(rest.trim());

    Some(Failure {
        tree_id: None,
        page: None,
        cell: None,
        kind,
        referenced_page: ref_page,
        raw_line: line.to_owned(),
    })
}

fn try_parse_page_level(line: &str) -> Option<Failure> {
    // "Page N: <message>"
    let rest = line.strip_prefix("Page ")?;
    let (page_str, rest) = rest.split_once(':')?;
    let page: u32 = page_str.trim().parse().ok()?;
    let msg = rest.trim();

    let kind = if msg.starts_with("never used") {
        FailureKind::PageNeverUsed
    } else if msg.starts_with("pointer map referenced") {
        FailureKind::PagePointerMapReferenced
    } else if msg.starts_with("Multiple uses for byte") {
        FailureKind::MultipleUsesForByte
    } else {
        FailureKind::Other(msg.to_owned())
    };

    Some(Failure {
        tree_id: None,
        page: Some(page),
        cell: None,
        kind,
        referenced_page: None,
        raw_line: line.to_owned(),
    })
}

fn try_parse_bare(line: &str) -> Option<Failure> {
    if line.starts_with("invalid page number") {
        let num = line
            .strip_prefix("invalid page number ")?
            .trim()
            .parse::<u32>()
            .ok();
        return Some(Failure {
            tree_id: None,
            page: num,
            cell: None,
            kind: FailureKind::InvalidPageNumber,
            referenced_page: None,
            raw_line: line.to_owned(),
        });
    }
    None
}

fn parse_message(msg: &str) -> (FailureKind, Option<u32>) {
    if let Some(rest) = msg.strip_prefix("2nd reference to page ") {
        let page: u32 = rest.trim().parse().unwrap_or(0);
        return (
            FailureKind::PageDoubleReference { ref_count: 2 },
            Some(page),
        );
    }

    if msg.contains("reference to page") {
        let ref_count = msg
            .split_whitespace()
            .next()
            .and_then(|s| {
                s.strip_suffix("th")
                    .or(s.strip_suffix("rd"))
                    .or(s.strip_suffix("nd"))
                    .or(s.strip_suffix("st"))
            })
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(2);
        let page = msg.rsplit(' ').next().and_then(|s| s.parse::<u32>().ok());
        return (FailureKind::PageDoubleReference { ref_count }, page);
    }

    if msg.starts_with("Rowid") && msg.contains("out of order") {
        return (FailureKind::RowidOutOfOrder, None);
    }
    if msg.starts_with("free space corruption") {
        return (FailureKind::FreeSpaceCorruption, None);
    }
    if msg == "Extends off end of page" {
        return (FailureKind::ExtendsOffEnd, None);
    }
    if msg.starts_with("Offset") && msg.contains("out of range") {
        let nums: Vec<u32> = msg
            .split(|c: char| !c.is_ascii_digit())
            .filter_map(|s| s.parse().ok())
            .collect();
        let (offset, lo, hi) = match nums.len() {
            3.. => (nums[0], nums[1], nums[2]),
            _ => (0, 0, 0),
        };
        return (
            FailureKind::OffsetOutOfRange {
                offset,
                range_lo: lo,
                range_hi: hi,
            },
            None,
        );
    }
    if msg == "Child page depth differs" {
        return (FailureKind::ChildPageDepthDiffers, None);
    }
    if msg.starts_with("freelist leaf count too big") {
        return (FailureKind::FreelistLeafCountTooBig, None);
    }
    if msg.starts_with("Failed to read ptrmap") {
        return (FailureKind::PtrmapReadFailed, None);
    }
    if msg.starts_with("failed to get page") {
        return (FailureKind::PageReadFailed, None);
    }

    (FailureKind::Other(msg.to_owned()), None)
}

#[must_use]
pub fn normalize(failure: &Failure) -> NormalizedSignature {
    let tree_id_class = failure.tree_id.map(TreeIdClass::from_id);

    let ref_count_class = match &failure.kind {
        FailureKind::PageDoubleReference { ref_count } => {
            Some(RefCountClass::from_count(ref_count.saturating_sub(1)))
        }
        _ => None,
    };

    let kind = match &failure.kind {
        FailureKind::OffsetOutOfRange { .. } => FailureKind::OffsetOutOfRange {
            offset: 0,
            range_lo: 0,
            range_hi: 0,
        },
        other => other.clone(),
    };

    NormalizedSignature {
        tree_id_class,
        kind,
        ref_count_class,
        has_cell: failure.cell.is_some(),
    }
}

pub fn fingerprint(failure_text: &str) -> Result<NormalizedSignature, ParseError> {
    match parse_failure(failure_text) {
        Some(f) => Ok(normalize(&f)),
        None => Err(ParseError {
            line_number: 0,
            message: "not a failure line".to_owned(),
        }),
    }
}

#[must_use]
pub fn fingerprint_collection(
    failure_lines: &[&str],
) -> HashMap<NormalizedSignature, Vec<Failure>> {
    let mut map: HashMap<NormalizedSignature, Vec<Failure>> = HashMap::new();
    for line in failure_lines {
        if let Some(failure) = parse_failure(line) {
            let sig = normalize(&failure);
            map.entry(sig).or_default().push(failure);
        }
    }
    map
}

#[must_use]
pub fn parse_integrity_check_output(output: &str) -> Vec<Failure> {
    output.lines().filter_map(parse_failure).collect()
}

#[must_use]
pub fn inventory_report(output: &str) -> String {
    let failures = parse_integrity_check_output(output);
    if failures.is_empty() {
        return "No integrity check failures found.\n".to_owned();
    }

    let mut grouped: HashMap<NormalizedSignature, Vec<&Failure>> = HashMap::new();
    for f in &failures {
        let sig = normalize(f);
        grouped.entry(sig).or_default().push(f);
    }

    let mut sigs: Vec<_> = grouped.into_iter().collect();
    sigs.sort_by(|a, b| b.1.len().cmp(&a.1.len()));

    let mut report = String::new();
    let _ = write!(
        report,
        "# Corruption Fingerprint Inventory\n\nTotal failures: {}\nDistinct signatures: {}\n\n",
        failures.len(),
        sigs.len()
    );

    for (i, (sig, examples)) in sigs.iter().enumerate() {
        let _ = write!(
            report,
            "## Signature {} — {} ({} occurrences)\n\n",
            i + 1,
            sig,
            examples.len()
        );

        let trees: Vec<_> = examples
            .iter()
            .filter_map(|f| f.tree_id)
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        if !trees.is_empty() {
            let _ = write!(report, "Trees: {trees:?}\n");
        }

        let pages: Vec<_> = examples
            .iter()
            .filter_map(|f| f.page)
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        if !pages.is_empty() {
            let _ = write!(report, "Pages: {pages:?}\n");
        }

        if examples.len() <= 3 {
            report.push_str("\nExamples:\n");
            for ex in examples {
                let _ = writeln!(report, "  {}", ex.raw_line);
            }
        } else {
            let _ = write!(report, "\nFirst 3 of {}:\n", examples.len());
            for ex in examples.iter().take(3) {
                let _ = writeln!(report, "  {}", ex.raw_line);
            }
        }
        report.push('\n');
    }

    report
}
