// SPDX-License-Identifier: AGPL-3.0-only
use std::collections::{BTreeMap, HashMap, HashSet};

use crate::index::IndexValue;
use crate::{DocumentId, Error, Result, codec::Cursor, format};

pub(crate) const PAGE_SIZE: usize = 4_096;
pub(crate) const PAGE_VERSION: u8 = 1;
pub(crate) const MAX_BTREE_PAGES: u64 = 32_768;
const PAGE_HEADER_SIZE: usize = 40;
const PAGE_CHECKSUM_OFFSET: usize = PAGE_SIZE - 4;
const PAGE_MAGIC: &[u8; 4] = b"SWIP";
const MAX_TREE_HEIGHT: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct TreeKey {
    pub(crate) value: IndexValue,
    pub(crate) document_id: DocumentId,
}

impl TreeKey {
    pub(crate) fn new(value: IndexValue, document_id: DocumentId) -> Self {
        Self { value, document_id }
    }
}

#[derive(Debug, Clone)]
enum Node {
    Leaf(Vec<TreeKey>),
    Internal {
        separators: Vec<TreeKey>,
        children: Vec<u64>,
    },
}

#[derive(Debug, Clone)]
struct Page {
    id: u64,
    index_id: i64,
    generation: u64,
    node: Node,
}

#[derive(Debug)]
pub(crate) struct TreeUpdate {
    pub(crate) root_page_id: u64,
    pub(crate) next_page_id: u64,
    pub(crate) pages: BTreeMap<u64, Vec<u8>>,
}

pub(crate) struct TreeBuilder<'a> {
    index_id: i64,
    generation: u64,
    committed: &'a HashMap<u64, Vec<u8>>,
    created: BTreeMap<u64, Vec<u8>>,
    next_page_id: u64,
}

impl<'a> TreeBuilder<'a> {
    pub(crate) fn new(
        index_id: i64,
        generation: u64,
        next_page_id: u64,
        committed: &'a HashMap<u64, Vec<u8>>,
    ) -> Result<Self> {
        if index_id <= 0 || generation > i64::MAX as u64 {
            return Err(Error::InvalidInput(
                "invalid B+Tree identity or generation".into(),
            ));
        }
        if next_page_id == 0 || next_page_id > MAX_BTREE_PAGES + 1 {
            return Err(Error::Limit("B+Tree page ID space is exhausted".into()));
        }
        Ok(Self {
            index_id,
            generation,
            committed,
            created: BTreeMap::new(),
            next_page_id,
        })
    }

    pub(crate) fn create_empty(&mut self) -> Result<u64> {
        self.allocate(Node::Leaf(Vec::new()))
    }

    pub(crate) fn insert(&mut self, root: u64, key: TreeKey) -> Result<u64> {
        let height = self.height(root)?;
        match self.insert_at(root, key)? {
            InsertResult::Single(root) => Ok(root),
            InsertResult::Split(left, separator, right) => {
                if height >= MAX_TREE_HEIGHT {
                    return Err(Error::Limit("B+Tree height limit reached".into()));
                }
                self.allocate(Node::Internal {
                    separators: vec![separator],
                    children: vec![left, right],
                })
            }
        }
    }

    pub(crate) fn remove(&mut self, root: u64, key: &TreeKey) -> Result<u64> {
        self.remove_at(root, key)
    }

    pub(crate) fn finish(self, root_page_id: u64) -> TreeUpdate {
        TreeUpdate {
            root_page_id,
            next_page_id: self.next_page_id,
            pages: self.created,
        }
    }

    fn insert_at(&mut self, page_id: u64, key: TreeKey) -> Result<InsertResult> {
        let mut page = self.load(page_id)?;
        match &mut page.node {
            Node::Leaf(keys) => match keys.binary_search(&key) {
                Ok(_) => Ok(InsertResult::Single(page_id)),
                Err(at) => {
                    keys.insert(at, key);
                    self.store_or_split(page.node)
                }
            },
            Node::Internal {
                separators,
                children,
            } => {
                let child_at = child_for(separators, &key);
                match self.insert_at(children[child_at], key)? {
                    InsertResult::Single(child) => {
                        if child == children[child_at] {
                            return Ok(InsertResult::Single(page_id));
                        }
                        children[child_at] = child;
                    }
                    InsertResult::Split(left, separator, right) => {
                        children[child_at] = left;
                        separators.insert(child_at, separator);
                        children.insert(child_at + 1, right);
                    }
                }
                self.store_or_split(page.node)
            }
        }
    }

    fn remove_at(&mut self, page_id: u64, key: &TreeKey) -> Result<u64> {
        let mut page = self.load(page_id)?;
        match &mut page.node {
            Node::Leaf(keys) => match keys.binary_search(key) {
                Ok(at) => {
                    keys.remove(at);
                    self.allocate(page.node)
                }
                Err(_) => Ok(page_id),
            },
            Node::Internal {
                separators,
                children,
            } => {
                let child_at = child_for(separators, key);
                let updated = self.remove_at(children[child_at], key)?;
                if updated == children[child_at] {
                    return Ok(page_id);
                }
                children[child_at] = updated;
                self.allocate(page.node)
            }
        }
    }

    fn store_or_split(&mut self, node: Node) -> Result<InsertResult> {
        match self.allocate(node.clone()) {
            Ok(page_id) => Ok(InsertResult::Single(page_id)),
            Err(Error::Limit(_)) => self.split(node),
            Err(error) => Err(error),
        }
    }

    fn split(&mut self, node: Node) -> Result<InsertResult> {
        match node {
            Node::Leaf(keys) => {
                for split_at in balanced_split_positions(keys.len()) {
                    let left = Node::Leaf(keys[..split_at].to_vec());
                    let right = Node::Leaf(keys[split_at..].to_vec());
                    if self.fits(&left)? && self.fits(&right)? {
                        let separator = keys[split_at].clone();
                        let left_id = self.allocate(left)?;
                        let right_id = self.allocate(right)?;
                        return Ok(InsertResult::Split(left_id, separator, right_id));
                    }
                }
                Err(Error::Limit(
                    "B+Tree leaf entry cannot fit on a page".into(),
                ))
            }
            Node::Internal {
                separators,
                children,
            } => {
                if separators.is_empty() || children.len() != separators.len() + 1 {
                    return Err(Error::Integrity("invalid B+Tree internal node".into()));
                }
                for split_at in balanced_split_positions(separators.len()) {
                    let promoted = separators[split_at].clone();
                    let left = Node::Internal {
                        separators: separators[..split_at].to_vec(),
                        children: children[..split_at + 1].to_vec(),
                    };
                    let right = Node::Internal {
                        separators: separators[split_at + 1..].to_vec(),
                        children: children[split_at + 1..].to_vec(),
                    };
                    if self.fits(&left)? && self.fits(&right)? {
                        let left_id = self.allocate(left)?;
                        let right_id = self.allocate(right)?;
                        return Ok(InsertResult::Split(left_id, promoted, right_id));
                    }
                }
                Err(Error::Limit("B+Tree internal node cannot be split".into()))
            }
        }
    }

    fn fits(&self, node: &Node) -> Result<bool> {
        match encode_page(self.next_page_id, self.index_id, self.generation, node) {
            Ok(_) => Ok(true),
            Err(Error::Limit(_)) => Ok(false),
            Err(error) => Err(error),
        }
    }

    fn allocate(&mut self, node: Node) -> Result<u64> {
        if self.next_page_id == 0 || self.next_page_id > MAX_BTREE_PAGES {
            return Err(Error::Limit("B+Tree page limit reached".into()));
        }
        let page_id = self.next_page_id;
        let encoded = encode_page(page_id, self.index_id, self.generation, &node)?;
        self.created.insert(page_id, encoded);
        self.next_page_id += 1;
        Ok(page_id)
    }

    fn load(&self, page_id: u64) -> Result<Page> {
        let bytes = self
            .created
            .get(&page_id)
            .or_else(|| self.committed.get(&page_id))
            .ok_or_else(|| Error::Integrity(format!("B+Tree page {page_id} is missing")))?;
        let page = decode_page(bytes)?;
        if page.id != page_id || page.index_id != self.index_id || page.generation > self.generation
        {
            return Err(Error::Integrity("B+Tree page identity mismatch".into()));
        }
        Ok(page)
    }

    fn height(&self, root_page_id: u64) -> Result<usize> {
        let mut current = root_page_id;
        let mut height = 0_usize;
        loop {
            height += 1;
            if height > MAX_TREE_HEIGHT {
                return Err(Error::Integrity("B+Tree height exceeds 32 pages".into()));
            }
            match self.load(current)?.node {
                Node::Leaf(_) => return Ok(height),
                Node::Internal { children, .. } => {
                    current = *children.first().ok_or_else(|| {
                        Error::Integrity("B+Tree internal node has no children".into())
                    })?;
                }
            }
        }
    }
}

enum InsertResult {
    Single(u64),
    Split(u64, TreeKey, u64),
}

fn balanced_split_positions(length: usize) -> Vec<usize> {
    if length < 2 {
        return if length == 1 { vec![0] } else { Vec::new() };
    }
    let middle = length / 2;
    let mut positions: Vec<_> = (0..length).collect();
    positions.sort_by_key(|position| position.abs_diff(middle));
    positions
}

fn child_for(separators: &[TreeKey], key: &TreeKey) -> usize {
    separators.partition_point(|separator| key >= separator)
}

pub(crate) fn build_tree(
    index_id: i64,
    generation: u64,
    next_page_id: u64,
    committed: &HashMap<u64, Vec<u8>>,
    keys: impl IntoIterator<Item = TreeKey>,
) -> Result<TreeUpdate> {
    let mut builder = TreeBuilder::new(index_id, generation, next_page_id, committed)?;
    let mut keys: Vec<_> = keys.into_iter().collect();
    keys.sort_unstable();
    if keys.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(Error::Integrity(
            "bulk B+Tree build received a duplicate key".into(),
        ));
    }
    if keys.is_empty() {
        let root = builder.create_empty()?;
        return Ok(builder.finish(root));
    }

    let mut leaves = Vec::new();
    let mut leaf_keys = Vec::new();
    for key in keys {
        leaf_keys.push(key);
        if !builder.fits(&Node::Leaf(leaf_keys.clone()))? {
            let last = leaf_keys.pop().expect("new key was just pushed");
            if leaf_keys.is_empty() {
                return Err(Error::Limit(
                    "B+Tree leaf entry cannot fit on a page".into(),
                ));
            }
            let minimum = leaf_keys[0].clone();
            let page_id = builder.allocate(Node::Leaf(std::mem::take(&mut leaf_keys)))?;
            leaves.push(BuildChild { page_id, minimum });
            leaf_keys.push(last);
            if !builder.fits(&Node::Leaf(leaf_keys.clone()))? {
                return Err(Error::Limit(
                    "B+Tree leaf entry cannot fit on a page".into(),
                ));
            }
        }
    }
    if !leaf_keys.is_empty() {
        let minimum = leaf_keys[0].clone();
        let page_id = builder.allocate(Node::Leaf(leaf_keys))?;
        leaves.push(BuildChild { page_id, minimum });
    }

    let mut level = leaves;
    while level.len() > 1 {
        let groups = partition_children(&builder, &level)?;
        let mut parents = Vec::with_capacity(groups.len());
        for (start, end) in groups {
            let children = &level[start..end];
            let separators = children
                .iter()
                .skip(1)
                .map(|child| child.minimum.clone())
                .collect();
            let page_id = builder.allocate(Node::Internal {
                separators,
                children: children.iter().map(|child| child.page_id).collect(),
            })?;
            parents.push(BuildChild {
                page_id,
                minimum: children[0].minimum.clone(),
            });
        }
        level = parents;
    }
    let root = level[0].page_id;
    Ok(builder.finish(root))
}

#[derive(Clone)]
struct BuildChild {
    page_id: u64,
    minimum: TreeKey,
}

fn partition_children(
    builder: &TreeBuilder<'_>,
    children: &[BuildChild],
) -> Result<Vec<(usize, usize)>> {
    let mut groups = Vec::new();
    let mut start = 0;
    while children.len() - start >= 2 {
        let mut end = start + 2;
        if !builder.fits(&internal_node(&children[start..end]))? {
            return Err(Error::Limit(
                "B+Tree internal separator cannot fit on a page".into(),
            ));
        }
        while end < children.len() && builder.fits(&internal_node(&children[start..=end]))? {
            end += 1;
        }
        groups.push((start, end));
        start = end;
    }
    if start < children.len() {
        let (previous_start, previous_end) = groups
            .pop()
            .ok_or_else(|| Error::Integrity("cannot group B+Tree child pages".into()))?;
        let count = children.len() - previous_start;
        if count < 4 {
            return Err(Error::Integrity(
                "B+Tree internal grouping left an unbalanced singleton".into(),
            ));
        }
        let mut left_sizes: Vec<_> = (2..=count - 2).collect();
        left_sizes.sort_by_key(|size| size.abs_diff(count / 2));
        let mut selected_split = None;
        for left_size in left_sizes {
            let split = previous_start + left_size;
            if builder.fits(&internal_node(&children[previous_start..split]))?
                && builder.fits(&internal_node(&children[split..]))?
            {
                selected_split = Some(split);
                break;
            }
        }
        let split = selected_split
            .ok_or_else(|| Error::Limit("B+Tree internal pages cannot be evenly grouped".into()))?;
        groups.push((previous_start, split));
        groups.push((split, children.len()));
        debug_assert_eq!(previous_end, children.len() - 1);
    }
    Ok(groups)
}

fn internal_node(children: &[BuildChild]) -> Node {
    Node::Internal {
        separators: children
            .iter()
            .skip(1)
            .map(|child| child.minimum.clone())
            .collect(),
        children: children.iter().map(|child| child.page_id).collect(),
    }
}

pub(crate) fn apply_changes(
    index_id: i64,
    root_page_id: u64,
    generation: u64,
    next_page_id: u64,
    committed: &HashMap<u64, Vec<u8>>,
    changes: impl IntoIterator<Item = (TreeKey, bool)>,
) -> Result<TreeUpdate> {
    let mut builder = TreeBuilder::new(index_id, generation, next_page_id, committed)?;
    let mut root = root_page_id;
    for (key, insert) in changes {
        root = if insert {
            builder.insert(root, key)?
        } else {
            builder.remove(root, &key)?
        };
    }
    Ok(builder.finish(root))
}

pub(crate) fn collect_entries(
    root_page_id: u64,
    index_id: i64,
    root_generation: u64,
    pages: &HashMap<u64, Vec<u8>>,
) -> Result<Vec<TreeKey>> {
    collect_entries_with_overlay(
        root_page_id,
        index_id,
        root_generation,
        pages,
        &BTreeMap::new(),
    )
}

pub(crate) fn collect_entries_with_overlay(
    root_page_id: u64,
    index_id: i64,
    root_generation: u64,
    pages: &HashMap<u64, Vec<u8>>,
    overlay: &BTreeMap<u64, Vec<u8>>,
) -> Result<Vec<TreeKey>> {
    let mut visited = HashSet::new();
    let mut entries = Vec::new();
    let bounds = visit_page(
        root_page_id,
        index_id,
        root_generation,
        pages,
        overlay,
        &mut visited,
        &mut entries,
        0,
        None,
        None,
    )?;
    let _ = bounds;
    Ok(entries)
}

#[derive(Default)]
struct Bounds {
    min: Option<TreeKey>,
    max: Option<TreeKey>,
}

#[allow(clippy::too_many_arguments)]
fn visit_page(
    page_id: u64,
    index_id: i64,
    root_generation: u64,
    pages: &HashMap<u64, Vec<u8>>,
    overlay: &BTreeMap<u64, Vec<u8>>,
    visited: &mut HashSet<u64>,
    entries: &mut Vec<TreeKey>,
    depth: usize,
    lower: Option<&TreeKey>,
    upper: Option<&TreeKey>,
) -> Result<Bounds> {
    if depth >= MAX_TREE_HEIGHT {
        return Err(Error::Integrity("B+Tree height exceeds 32 pages".into()));
    }
    if !visited.insert(page_id) {
        return Err(Error::Integrity(
            "B+Tree contains a cycle or shared child".into(),
        ));
    }
    if visited.len() as u64 > MAX_BTREE_PAGES {
        return Err(Error::Integrity(
            "B+Tree page count exceeds the limit".into(),
        ));
    }
    let bytes = overlay
        .get(&page_id)
        .or_else(|| pages.get(&page_id))
        .ok_or_else(|| Error::Integrity(format!("B+Tree references missing page {page_id}")))?;
    let page = decode_page(bytes)?;
    if page.id != page_id || page.index_id != index_id || page.generation > root_generation {
        return Err(Error::Integrity(
            "B+Tree page metadata does not match its root".into(),
        ));
    }
    match page.node {
        Node::Leaf(keys) => {
            if keys.iter().any(|key| {
                lower.is_some_and(|bound| key < bound) || upper.is_some_and(|bound| key >= bound)
            }) {
                return Err(Error::Integrity(
                    "B+Tree leaf key is outside its page range".into(),
                ));
            }
            if let (Some(lower), Some(first)) = (lower, keys.first())
                && first < lower
            {
                return Err(Error::Integrity(
                    "B+Tree leaf violates its lower fence".into(),
                ));
            }
            let bounds = Bounds {
                min: keys.first().cloned(),
                max: keys.last().cloned(),
            };
            entries.extend(keys);
            Ok(bounds)
        }
        Node::Internal {
            separators,
            children,
        } => {
            if children.len() != separators.len() + 1 || children.is_empty() {
                return Err(Error::Integrity("B+Tree child count is invalid".into()));
            }
            let mut aggregate = Bounds::default();
            let mut previous_max: Option<TreeKey> = None;
            for (position, child) in children.iter().enumerate() {
                let child_lower = if position == 0 {
                    lower
                } else {
                    Some(&separators[position - 1])
                };
                let child_upper = if position == separators.len() {
                    upper
                } else {
                    Some(&separators[position])
                };
                let child_bounds = visit_page(
                    *child,
                    index_id,
                    root_generation,
                    pages,
                    overlay,
                    visited,
                    entries,
                    depth + 1,
                    child_lower,
                    child_upper,
                )?;
                if let Some(max) = &child_bounds.max {
                    if previous_max
                        .as_ref()
                        .is_some_and(|previous| previous >= max)
                    {
                        return Err(Error::Integrity("B+Tree child ranges overlap".into()));
                    }
                    if position < separators.len() && max >= &separators[position] {
                        return Err(Error::Integrity(
                            "B+Tree separator is below its left child".into(),
                        ));
                    }
                    previous_max = Some(max.clone());
                    aggregate.max = Some(max.clone());
                }
                if aggregate.min.is_none() {
                    aggregate.min = child_bounds.min.clone();
                }
                if position > 0
                    && child_bounds
                        .min
                        .as_ref()
                        .is_some_and(|minimum| minimum < &separators[position - 1])
                {
                    return Err(Error::Integrity(
                        "B+Tree separator exceeds its right child".into(),
                    ));
                }
            }
            Ok(aggregate)
        }
    }
}

pub(crate) fn decode_stored_page(bytes: &[u8]) -> Result<(u64, i64, u64)> {
    let page = decode_page(bytes)?;
    Ok((page.id, page.index_id, page.generation))
}

fn encode_page(page_id: u64, index_id: i64, generation: u64, node: &Node) -> Result<Vec<u8>> {
    if page_id == 0 || page_id > MAX_BTREE_PAGES || index_id <= 0 || generation > i64::MAX as u64 {
        return Err(Error::Integrity("invalid B+Tree page identity".into()));
    }
    let mut payload = Vec::new();
    let (page_type, keys) = match node {
        Node::Leaf(keys) => {
            for key in keys {
                encode_key(key, &mut payload)?;
            }
            (1_u8, keys.len())
        }
        Node::Internal {
            separators,
            children,
        } => {
            if children.len() != separators.len() + 1 || children.is_empty() {
                return Err(Error::Integrity("invalid B+Tree internal page".into()));
            }
            payload.extend_from_slice(&encode_page_id(children[0])?);
            for (separator, child) in separators.iter().zip(&children[1..]) {
                encode_key(separator, &mut payload)?;
                payload.extend_from_slice(&encode_page_id(*child)?);
            }
            (2_u8, separators.len())
        }
    };
    if keys > u16::MAX as usize {
        return Err(Error::Limit("B+Tree page key count exceeds u16".into()));
    }
    let max_payload = PAGE_CHECKSUM_OFFSET - PAGE_HEADER_SIZE;
    if payload.len() > max_payload || payload.len() > u16::MAX as usize {
        return Err(Error::Limit("B+Tree page payload does not fit".into()));
    }
    let mut page = vec![0_u8; PAGE_SIZE];
    page[..4].copy_from_slice(PAGE_MAGIC);
    page[4] = PAGE_VERSION;
    page[5] = page_type;
    page[8..16].copy_from_slice(&page_id.to_be_bytes());
    page[16..24].copy_from_slice(&index_id.to_be_bytes());
    page[24..32].copy_from_slice(&generation.to_be_bytes());
    page[32..34].copy_from_slice(&(keys as u16).to_be_bytes());
    page[34..36].copy_from_slice(&(payload.len() as u16).to_be_bytes());
    page[PAGE_HEADER_SIZE..PAGE_HEADER_SIZE + payload.len()].copy_from_slice(&payload);
    let checksum = format::crc32c(&page[..PAGE_CHECKSUM_OFFSET]);
    page[PAGE_CHECKSUM_OFFSET..].copy_from_slice(&checksum.to_be_bytes());
    Ok(page)
}

fn decode_page(bytes: &[u8]) -> Result<Page> {
    if bytes.len() != PAGE_SIZE {
        return Err(Error::Integrity(
            "B+Tree page must be exactly 4096 bytes".into(),
        ));
    }
    if &bytes[..4] != PAGE_MAGIC {
        return Err(Error::Format("invalid B+Tree page magic".into()));
    }
    if bytes[4] != PAGE_VERSION {
        return Err(Error::Format(format!(
            "unsupported B+Tree page version {}",
            bytes[4]
        )));
    }
    if bytes[6..8] != [0, 0] || bytes[36..PAGE_HEADER_SIZE].iter().any(|byte| *byte != 0) {
        return Err(Error::Format(
            "B+Tree page reserved bytes must be zero".into(),
        ));
    }
    let stored = u32::from_be_bytes(
        bytes[PAGE_CHECKSUM_OFFSET..]
            .try_into()
            .expect("fixed page checksum"),
    );
    if stored != format::crc32c(&bytes[..PAGE_CHECKSUM_OFFSET]) {
        return Err(Error::Integrity("B+Tree page checksum mismatch".into()));
    }
    let id = read_u64(&bytes[8..16]);
    let index_id = i64::from_be_bytes(bytes[16..24].try_into().expect("fixed page index ID"));
    let generation = read_u64(&bytes[24..32]);
    if id == 0 || id > MAX_BTREE_PAGES || index_id <= 0 || generation > i64::MAX as u64 {
        return Err(Error::Integrity(
            "B+Tree page identity is out of bounds".into(),
        ));
    }
    let key_count = usize::from(u16::from_be_bytes(
        bytes[32..34].try_into().expect("fixed key count"),
    ));
    let payload_len = usize::from(u16::from_be_bytes(
        bytes[34..36].try_into().expect("fixed payload length"),
    ));
    let payload_end = PAGE_HEADER_SIZE
        .checked_add(payload_len)
        .ok_or_else(|| Error::Integrity("B+Tree payload length overflow".into()))?;
    if payload_end > PAGE_CHECKSUM_OFFSET {
        return Err(Error::Integrity(
            "B+Tree payload length is out of bounds".into(),
        ));
    }
    if bytes[payload_end..PAGE_CHECKSUM_OFFSET]
        .iter()
        .any(|byte| *byte != 0)
    {
        return Err(Error::Format("B+Tree page padding must be zero".into()));
    }
    let mut input = Cursor::new(&bytes[PAGE_HEADER_SIZE..payload_end]);
    let node = match bytes[5] {
        1 => {
            if key_count > input.remaining() / 9 {
                return Err(Error::Integrity(
                    "B+Tree leaf key count is impossible".into(),
                ));
            }
            let mut keys = Vec::with_capacity(key_count);
            for _ in 0..key_count {
                keys.push(decode_key(&mut input)?);
            }
            if keys.windows(2).any(|pair| pair[0] >= pair[1]) {
                return Err(Error::Integrity(
                    "B+Tree leaf keys are not strictly ordered".into(),
                ));
            }
            Node::Leaf(keys)
        }
        2 => {
            if key_count == 0 || key_count > input.remaining() / 17 {
                return Err(Error::Integrity(
                    "B+Tree internal key count is impossible".into(),
                ));
            }
            let first_child = decode_page_id(&mut input)?;
            let mut separators = Vec::with_capacity(key_count);
            let mut children = Vec::with_capacity(key_count + 1);
            children.push(first_child);
            for _ in 0..key_count {
                separators.push(decode_key(&mut input)?);
                children.push(decode_page_id(&mut input)?);
            }
            if separators.windows(2).any(|pair| pair[0] >= pair[1]) {
                return Err(Error::Integrity(
                    "B+Tree separators are not strictly ordered".into(),
                ));
            }
            Node::Internal {
                separators,
                children,
            }
        }
        page_type => {
            return Err(Error::Format(format!(
                "invalid B+Tree page type {page_type}"
            )));
        }
    };
    input.finish("B+Tree page payload")?;
    Ok(Page {
        id,
        index_id,
        generation,
        node,
    })
}

fn encode_key(key: &TreeKey, output: &mut Vec<u8>) -> Result<()> {
    key.value.encode(output)?;
    output.extend_from_slice(&key.document_id.value().to_be_bytes());
    Ok(())
}

fn decode_key(input: &mut Cursor<'_>) -> Result<TreeKey> {
    let value = IndexValue::decode(input)?;
    let document_id = DocumentId::new(input.i64("B+Tree document identifier")?)
        .map_err(|_| Error::Integrity("B+Tree document identifier must be positive".into()))?;
    Ok(TreeKey { value, document_id })
}

fn encode_page_id(page_id: u64) -> Result<[u8; 8]> {
    if page_id == 0 || page_id > MAX_BTREE_PAGES {
        return Err(Error::Integrity(
            "B+Tree child page ID is out of bounds".into(),
        ));
    }
    Ok(page_id.to_be_bytes())
}

fn decode_page_id(input: &mut Cursor<'_>) -> Result<u64> {
    let page_id = read_u64(&input.bytes_exact(8, "B+Tree child page ID")?);
    if page_id == 0 || page_id > MAX_BTREE_PAGES {
        return Err(Error::Integrity(
            "B+Tree child page ID is out of bounds".into(),
        ));
    }
    Ok(page_id)
}

fn read_u64(bytes: &[u8]) -> u64 {
    u64::from_be_bytes(bytes.try_into().expect("fixed u64 range"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Value;

    #[test]
    fn copy_on_write_pages_round_trip_and_keep_old_root_readable() {
        let pages = HashMap::new();
        let mut builder = TreeBuilder::new(1, 0, 1, &pages).unwrap();
        let old_root = builder.create_empty().unwrap();
        let mut root = old_root;
        for id in 1..400 {
            root = builder
                .insert(
                    root,
                    TreeKey::new(
                        IndexValue::from_value(&Value::Integer((id * 17) % 401)).unwrap(),
                        DocumentId::new(id).unwrap(),
                    ),
                )
                .unwrap();
        }
        let update = builder.finish(root);
        let mut all_pages = update.pages.clone().into_iter().collect::<HashMap<_, _>>();
        assert!(all_pages.contains_key(&old_root));
        let entries = collect_entries(update.root_page_id, 1, 0, &all_pages).unwrap();
        assert_eq!(entries.len(), 399);
        assert!(entries.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(
            collect_entries(old_root, 1, 0, &all_pages)
                .unwrap()
                .is_empty()
        );

        let mut next = update.next_page_id;
        let changed = apply_changes(
            1,
            update.root_page_id,
            1,
            next,
            &all_pages,
            [(
                TreeKey::new(
                    IndexValue::from_value(&Value::Integer(17)).unwrap(),
                    DocumentId::new(1).unwrap(),
                ),
                false,
            )],
        )
        .unwrap();
        next = changed.next_page_id;
        assert!(next > update.next_page_id);
        all_pages.extend(changed.pages);
        let entries = collect_entries(changed.root_page_id, 1, 1, &all_pages).unwrap();
        assert_eq!(entries.len(), 398);
        assert!(!entries.iter().any(|key| key.document_id.value() == 1));
    }

    #[test]
    fn bulk_build_packs_pages_instead_of_retaining_intermediate_roots() {
        let mut pages = HashMap::new();
        let keys = (1..=5_000).map(|id| {
            TreeKey::new(
                IndexValue::from_value(&Value::Integer(id % 1_000)).unwrap(),
                DocumentId::new(id).unwrap(),
            )
        });
        let update = build_tree(1, 0, 1, &pages, keys).unwrap();
        assert!(update.pages.len() < 64);
        pages.extend(update.pages);
        assert_eq!(
            collect_entries(update.root_page_id, 1, 0, &pages)
                .unwrap()
                .len(),
            5_000
        );
    }

    #[test]
    fn rejects_corrupt_page_checksum_and_truncated_page() {
        let mut pages = HashMap::new();
        let update = build_tree(1, 0, 1, &pages, []).unwrap();
        pages.extend(update.pages);
        let page = pages.get_mut(&update.root_page_id).unwrap();
        page[PAGE_HEADER_SIZE] = 1;
        assert!(matches!(
            collect_entries(update.root_page_id, 1, 0, &pages),
            Err(Error::Integrity(_))
        ));
        assert!(matches!(decode_page(&[0; 20]), Err(Error::Integrity(_))));
    }

    #[test]
    fn rejects_invalid_type_count_root_reference_and_cycle() {
        let mut pages = HashMap::new();
        let mut bad_type = encode_page(1, 1, 0, &Node::Leaf(Vec::new())).unwrap();
        bad_type[5] = 99;
        let checksum = format::crc32c(&bad_type[..PAGE_CHECKSUM_OFFSET]);
        bad_type[PAGE_CHECKSUM_OFFSET..].copy_from_slice(&checksum.to_be_bytes());
        assert!(matches!(decode_page(&bad_type), Err(Error::Format(_))));

        let mut bad_count = encode_page(2, 1, 0, &Node::Leaf(Vec::new())).unwrap();
        bad_count[32..34].copy_from_slice(&1_u16.to_be_bytes());
        let checksum = format::crc32c(&bad_count[..PAGE_CHECKSUM_OFFSET]);
        bad_count[PAGE_CHECKSUM_OFFSET..].copy_from_slice(&checksum.to_be_bytes());
        assert!(matches!(decode_page(&bad_count), Err(Error::Integrity(_))));

        assert!(matches!(
            collect_entries(999, 1, 0, &pages),
            Err(Error::Integrity(_))
        ));

        assert!(matches!(
            encode_page(
                3,
                1,
                0,
                &Node::Internal {
                    separators: vec![TreeKey::new(
                        IndexValue::from_value(&Value::Integer(10)).unwrap(),
                        DocumentId::new(1).unwrap(),
                    )],
                    children: vec![1, 0],
                },
            ),
            Err(Error::Integrity(_))
        ));

        let mut missing_child_pages = HashMap::new();
        missing_child_pages.insert(1, encode_page(1, 1, 0, &Node::Leaf(Vec::new())).unwrap());
        missing_child_pages.insert(2, encode_page(2, 1, 0, &Node::Leaf(Vec::new())).unwrap());
        missing_child_pages.insert(
            3,
            encode_page(
                3,
                1,
                0,
                &Node::Internal {
                    separators: vec![TreeKey::new(
                        IndexValue::from_value(&Value::Integer(10)).unwrap(),
                        DocumentId::new(1).unwrap(),
                    )],
                    children: vec![1, 4],
                },
            )
            .unwrap(),
        );
        assert!(matches!(
            collect_entries(3, 1, 0, &missing_child_pages),
            Err(Error::Integrity(_))
        ));

        let separator = TreeKey::new(
            IndexValue::from_value(&Value::Integer(10)).unwrap(),
            DocumentId::new(1).unwrap(),
        );
        pages.insert(
            1,
            encode_page(
                1,
                1,
                0,
                &Node::Internal {
                    separators: vec![separator],
                    children: vec![1, 2],
                },
            )
            .unwrap(),
        );
        pages.insert(2, encode_page(2, 1, 0, &Node::Leaf(Vec::new())).unwrap());
        assert!(matches!(
            collect_entries(1, 1, 0, &pages),
            Err(Error::Integrity(_))
        ));

        pages.insert(
            1,
            encode_page(
                1,
                1,
                0,
                &Node::Internal {
                    separators: vec![TreeKey::new(
                        IndexValue::from_value(&Value::Integer(10)).unwrap(),
                        DocumentId::new(1).unwrap(),
                    )],
                    children: vec![3, 2],
                },
            )
            .unwrap(),
        );
        assert!(matches!(
            collect_entries(1, 1, 0, &pages),
            Err(Error::Integrity(_))
        ));
    }

    proptest::proptest! {
        #![proptest_config(proptest::test_runner::Config {
            cases: 48,
            max_shrink_iters: 2_000,
            .. proptest::test_runner::Config::default()
        })]

        #[test]
        fn copy_on_write_tree_matches_ordered_set_oracle(
            operations in proptest::collection::vec(
                (proptest::prelude::any::<bool>(), -20_i64..=80, 1_i64..=64),
                0..=384
            )
        ) {
            let mut pages = HashMap::new();
            let initial = build_tree(1, 0, 1, &pages, []).unwrap();
            let mut root = initial.root_page_id;
            let mut next_page_id = initial.next_page_id;
            pages.extend(initial.pages);
            let mut oracle = std::collections::BTreeSet::new();

            for (batch_number, batch) in operations.chunks(24).enumerate() {
                let generation = batch_number as u64 + 1;
                let changes = batch.iter().map(|(insert, value, document_id)| {
                    let key = TreeKey::new(
                        IndexValue::from_value(&Value::Integer(*value)).unwrap(),
                        DocumentId::new(*document_id).unwrap(),
                    );
                    if *insert {
                        oracle.insert(key.clone());
                    } else {
                        oracle.remove(&key);
                    }
                    (key, *insert)
                });
                let update = apply_changes(
                    1,
                    root,
                    generation,
                    next_page_id,
                    &pages,
                    changes,
                )
                .unwrap();
                root = update.root_page_id;
                next_page_id = update.next_page_id;
                pages.extend(update.pages);
                proptest::prop_assert_eq!(collect_entries(root, 1, generation, &pages).unwrap(),
                    oracle.iter().cloned().collect::<Vec<_>>());
            }
        }
    }
}
