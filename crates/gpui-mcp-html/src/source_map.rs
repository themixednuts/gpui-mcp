//! Map rendered elements back to the HTML that produced them.
//!
//! Every element is mapped, whether or not it has an authored `id`: elements
//! without one are rendered under a generated id derived from their position,
//! and each element also carries the byte range of its markup in the source.

use std::collections::HashMap;
use std::ops::Range;

use htmlswap::{RenderNode, RenderPlan};

use crate::ElementId;
use crate::document::attribute;

/// Where one rendered element came from.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceNode {
    /// Identity of the element in the semantic UI tree, as MCP tools and
    /// [`gpui_mcp::Automation::snapshot`] report it. Includes the embedding
    /// namespace, if any.
    pub semantic_id: String,
    /// The element's document id: its authored `id`, or a generated
    /// `html-node-…` id. Bindings target this id.
    pub element_id: ElementId,
    /// The authored `id` attribute, if the element has one.
    pub authored_id: Option<String>,
    /// Lowercase tag name as written.
    pub tag: String,
    /// Child indices from the document root to this element, counting text
    /// nodes. Generated ids are derived from it, so they are stable while the
    /// element keeps its position.
    pub path: Vec<usize>,
    /// Semantic id of the nearest enclosing element.
    pub parent: Option<String>,
    /// Byte range of the element's markup in the HTML source, from the start
    /// of its start tag to the end of its end tag.
    pub span: Option<Range<usize>>,
    /// One-based line of the start tag.
    pub line: Option<usize>,
    /// One-based column, in characters, of the start tag.
    pub column: Option<usize>,
}

/// Rendered elements of one document revision, in document order.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SourceMap {
    nodes: Vec<SourceNode>,
    by_semantic_id: HashMap<String, usize>,
}

impl SourceMap {
    pub(crate) fn build(
        plan: &RenderPlan,
        source: &str,
        semantic_id: impl Fn(&str) -> String,
    ) -> Self {
        let lines = LineIndex::new(source);
        let mut map = Self::default();
        collect(&plan.nodes, &[], None, &semantic_id, &lines, &mut map);
        map
    }

    /// Every rendered element in document order.
    #[must_use]
    pub fn nodes(&self) -> &[SourceNode] {
        &self.nodes
    }

    /// The element with this semantic id.
    #[must_use]
    pub fn get(&self, semantic_id: &str) -> Option<&SourceNode> {
        self.by_semantic_id
            .get(semantic_id)
            .map(|index| &self.nodes[*index])
    }

    /// The innermost element whose markup contains this byte offset, for
    /// example the element under an editor's caret.
    #[must_use]
    pub fn at_offset(&self, offset: usize) -> Option<&SourceNode> {
        self.nodes
            .iter()
            .filter(|node| {
                node.span
                    .as_ref()
                    .is_some_and(|span| span.start <= offset && offset < span.end)
            })
            .min_by_key(|node| {
                node.span
                    .as_ref()
                    .map_or(usize::MAX, ExactSizeIterator::len)
            })
    }
}

fn collect(
    nodes: &[RenderNode],
    parent_path: &[usize],
    parent: Option<&str>,
    semantic_id: &impl Fn(&str) -> String,
    lines: &LineIndex,
    map: &mut SourceMap,
) {
    for (index, node) in nodes.iter().enumerate() {
        let RenderNode::Element(element) = node else {
            continue;
        };
        let mut path = parent_path.to_vec();
        path.push(index);
        let authored_id = attribute(element, "id").map(str::to_owned);
        let id = authored_id
            .clone()
            .unwrap_or_else(|| crate::render::generated_id(&path));
        let semantic = semantic_id(&id);
        let span = element.span.map(|span| span.start..span.end);
        let (line, column) = span
            .as_ref()
            .and_then(|span| lines.position(span.start))
            .map_or((None, None), |(line, column)| (Some(line), Some(column)));
        map.by_semantic_id.insert(semantic.clone(), map.nodes.len());
        map.nodes.push(SourceNode {
            semantic_id: semantic.clone(),
            element_id: ElementId::new(id),
            authored_id,
            tag: element.source_tag.to_string(),
            path: path.clone(),
            parent: parent.map(ToOwned::to_owned),
            span,
            line,
            column,
        });
        collect(
            &element.children,
            &path,
            Some(&semantic),
            semantic_id,
            lines,
            map,
        );
    }
}

/// Byte offsets of line starts, for converting offsets to line and column.
struct LineIndex<'a> {
    source: &'a str,
    starts: Vec<usize>,
}

impl<'a> LineIndex<'a> {
    fn new(source: &'a str) -> Self {
        let starts = std::iter::once(0)
            .chain(source.match_indices('\n').map(|(index, _)| index + 1))
            .collect();
        Self { source, starts }
    }

    fn position(&self, offset: usize) -> Option<(usize, usize)> {
        if offset > self.source.len() || !self.source.is_char_boundary(offset) {
            return None;
        }
        let line = self.starts.partition_point(|start| *start <= offset) - 1;
        let column = self.source[self.starts[line]..offset].chars().count() + 1;
        Some((line + 1, column))
    }
}

#[cfg(test)]
mod tests {
    use super::LineIndex;

    #[test]
    fn offsets_become_one_based_lines_and_character_columns() {
        let lines = LineIndex::new("ab\né<x>\n");
        assert_eq!(lines.position(0), Some((1, 1)));
        assert_eq!(lines.position(3), Some((2, 1)));
        assert_eq!(lines.position(5), Some((2, 2)), "é is one character");
        assert_eq!(lines.position(4), None, "inside a character");
        assert_eq!(lines.position(99), None);
    }
}
