use quick_xml::events::BytesStart;
use std::io::Read;

/// Number of bytes requested from the underlying reader at once.
const READ_BYTES: usize = 1024 * 1024;

/// Splits an XML stream into the raw bytes of all elements with a given local name, e.g., all
/// `person` elements of a population. The elements can then be parsed independently of each other.
///
/// The splitter doesn't parse the document. It relies on `<` never appearing unescaped in
/// attribute values or text, so that every `<` outside of comments, CDATA sections, processing
/// instructions and declarations starts a tag. Elements with the given name must not be nested.
///
/// As a simple check that the input is complete, the root element must be closed at the end of
/// the input. Namespace declarations of the root element are added to elements which don't declare
/// namespaces themselves, so that the elements can be parsed on their own.
pub(crate) struct ElementSplitter<R> {
    reader: R,
    name: Vec<u8>,
    buffer: Vec<u8>,
    // Position in `buffer` up to which the input has been scanned.
    pos: usize,
    // Start of the element which is currently being split off.
    element_start: Option<usize>,
    // Length of the qualified name of the element which is currently being split off.
    element_name_len: usize,
    // Qualified name of the root element.
    root: Option<Vec<u8>>,
    root_closed: bool,
    // Namespace declarations of the root element, each preceded by a space.
    namespaces: Vec<u8>,
    eof: bool,
}

/// A piece of markup starting with `<`.
enum Markup {
    /// Start tag of an element with the searched name. `name_len` is the length of its qualified
    /// name.
    Start { self_closing: bool, name_len: usize },
    /// End tag of an element with the searched name.
    End,
    /// Start tag of the root element.
    RootStart { self_closing: bool, name_len: usize },
    /// End tag of the root element.
    RootEnd,
    /// Any other markup, which is skipped.
    Other,
}

impl<R: Read> ElementSplitter<R> {
    pub(crate) fn new(reader: R, name: &str) -> Self {
        Self {
            reader,
            name: name.as_bytes().to_vec(),
            buffer: Vec::new(),
            pos: 0,
            element_start: None,
            element_name_len: 0,
            root: None,
            root_closed: false,
            namespaces: Vec::new(),
            eof: false,
        }
    }

    /// Appends the bytes of the next element, including its start and end tag, to `out`. Returns
    /// `false` if there are no more elements.
    pub(crate) fn next_element_into(&mut self, out: &mut Vec<u8>) -> bool {
        loop {
            if let Some((start, end)) = self.scan() {
                let element = &self.buffer[start..end];
                let name_end = 1 + self.element_name_len;
                let start_tag_len = find_tag_end(element).unwrap();
                if self.namespaces.is_empty()
                    || find_seq(&element[..start_tag_len], b"xmlns").is_some()
                {
                    out.extend_from_slice(element);
                } else {
                    out.extend_from_slice(&element[..name_end]);
                    out.extend_from_slice(&self.namespaces);
                    out.extend_from_slice(&element[name_end..]);
                }
                return true;
            }
            if self.eof {
                assert!(
                    self.element_start.is_none(),
                    "Input ended within a <{}> element.",
                    String::from_utf8_lossy(&self.name)
                );
                assert!(
                    self.root_closed,
                    "Input ended before the end of the root element."
                );
                return false;
            }
            self.fill();
        }
    }

    /// Scans the buffered input for the end of the next element and returns its range in the
    /// buffer. Returns `None` if more input is needed.
    fn scan(&mut self) -> Option<(usize, usize)> {
        loop {
            let lt = self.pos + find_byte(&self.buffer[self.pos..], b'<')?;
            let Some((markup, len)) = self.classify(lt) else {
                // The markup is incomplete. Continue at its start when more input is available.
                self.pos = lt;
                return None;
            };
            let end = lt + len;
            self.pos = end;

            match markup {
                Markup::Start {
                    self_closing,
                    name_len,
                } => {
                    assert!(
                        self.element_start.is_none(),
                        "Nested <{}> elements are not supported.",
                        String::from_utf8_lossy(&self.name)
                    );
                    self.element_name_len = name_len;
                    if self_closing {
                        return Some((lt, end));
                    }
                    self.element_start = Some(lt);
                }
                Markup::End => {
                    let start = self.element_start.take().unwrap_or_else(|| {
                        panic!(
                            "Found </{}> without a matching start tag.",
                            String::from_utf8_lossy(&self.name)
                        )
                    });
                    return Some((start, end));
                }
                Markup::RootStart {
                    self_closing,
                    name_len,
                } => {
                    let tag = &self.buffer[lt..end];
                    self.root = Some(tag[1..1 + name_len].to_vec());
                    self.namespaces = namespace_declarations(tag, name_len);
                    self.root_closed = self_closing;
                }
                Markup::RootEnd => self.root_closed = true,
                Markup::Other => {}
            }
        }
    }

    /// Classifies the markup starting with the `<` at `lt` and returns it together with the
    /// number of bytes which can be skipped. Returns `None` if the markup is incomplete.
    fn classify(&self, lt: usize) -> Option<(Markup, usize)> {
        let rest = &self.buffer[lt..];
        if let Some(len) = skip_special(rest) {
            return len.map(|len| (Markup::Other, len));
        }

        let is_end = rest.get(1) == Some(&b'/');
        let name_start = if is_end { 2 } else { 1 };
        let Some(name_len) = rest[name_start..]
            .iter()
            .position(|&b| b.is_ascii_whitespace() || b == b'>' || b == b'/')
        else {
            // The name is incomplete. At the end of the input, the truncated tag is skipped, so
            // that the missing end of the root element is reported.
            return self.eof.then_some((Markup::Other, rest.len()));
        };
        let name = &rest[name_start..name_start + name_len];
        let is_element = local_name(name) == self.name.as_slice();
        if !is_element {
            let outside = self.element_start.is_none();
            match (&self.root, is_end) {
                // The first start tag outside of split elements is the one of the root element.
                (None, false) if outside => {}
                (Some(root), true) if outside && root == name => {
                    return Some((Markup::RootEnd, 1));
                }
                // Other tags may contain `>` in attribute values but never `<`, so scanning can
                // continue right after the `<`.
                _ => return Some((Markup::Other, 1)),
            }
        }

        let Some(tag_len) = find_tag_end(&rest[name_start + name_len..]) else {
            assert!(
                !self.eof,
                "Input ended within a <{}> element.",
                String::from_utf8_lossy(name)
            );
            return None;
        };
        let len = name_start + name_len + tag_len;
        let self_closing = rest[len - 2] == b'/';
        let markup = match (is_element, is_end) {
            (true, true) => Markup::End,
            (true, false) => Markup::Start {
                self_closing,
                name_len,
            },
            (false, _) => Markup::RootStart {
                self_closing,
                name_len,
            },
        };
        Some((markup, len))
    }

    fn fill(&mut self) {
        // Drop the input which has been scanned and doesn't belong to the current element.
        let keep_from = self.element_start.unwrap_or(self.pos);
        if keep_from > 0 {
            self.buffer.drain(..keep_from);
            self.pos -= keep_from;
            self.element_start = self.element_start.map(|start| start - keep_from);
        }

        // Readers like decompressors may return only a few bytes per call. Reading until
        // READ_BYTES are available keeps the number of scans and buffer moves small.
        let read = (&mut self.reader)
            .take(READ_BYTES as u64)
            .read_to_end(&mut self.buffer)
            .unwrap_or_else(|e| panic!("Failed to read XML input: {e}"));
        self.eof = read == 0;
    }
}

/// Returns the part of a qualified name after the namespace prefix.
fn local_name(name: &[u8]) -> &[u8] {
    match name.iter().rposition(|&b| b == b':') {
        Some(colon) => &name[colon + 1..],
        None => name,
    }
}

/// Returns the namespace declarations of a start tag, each preceded by a space.
fn namespace_declarations(tag: &[u8], name_len: usize) -> Vec<u8> {
    let content_end = tag.len() - if tag.ends_with(b"/>") { 2 } else { 1 };
    let content = String::from_utf8_lossy(&tag[1..content_end]);
    let mut result = Vec::new();
    for attribute in BytesStart::from_content(content, name_len).attributes() {
        let attribute = attribute.expect("Invalid attribute of the root element.");
        let key = attribute.key.as_ref();
        if key == b"xmlns" || key.starts_with(b"xmlns:") {
            // The value is still escaped, so it may only contain the quote, which wasn't used.
            let quote = if attribute.value.contains(&b'"') {
                b'\''
            } else {
                b'"'
            };
            result.push(b' ');
            result.extend_from_slice(key);
            result.extend_from_slice(&[b'=', quote]);
            result.extend_from_slice(&attribute.value);
            result.push(quote);
        }
    }
    result
}

/// Handles comments, CDATA sections, processing instructions and declarations like DOCTYPE.
/// Returns `None` if `markup` starts with something else, `Some(None)` if it is incomplete and
/// `Some(Some(len))` with its length otherwise.
fn skip_special(markup: &[u8]) -> Option<Option<usize>> {
    const SPECIAL: [(&[u8], &[u8]); 3] =
        [(b"<!--", b"-->"), (b"<![CDATA[", b"]]>"), (b"<?", b"?>")];
    for (start, end) in SPECIAL {
        if markup.starts_with(start) {
            let len = find_seq(&markup[start.len()..], end).map(|i| start.len() + i + end.len());
            return Some(len);
        }
        if start.starts_with(markup) {
            // Too short to decide.
            return Some(None);
        }
    }
    if markup.starts_with(b"<!") {
        return Some(find_declaration_end(markup));
    }
    None
}

/// Returns the length of a tag up to and including its closing `>`, which must not be part of a
/// quoted attribute value.
fn find_tag_end(tag: &[u8]) -> Option<usize> {
    let mut quote = None;
    for (i, &byte) in tag.iter().enumerate() {
        match quote {
            Some(q) if byte == q => quote = None,
            Some(_) => {}
            None if byte == b'"' || byte == b'\'' => quote = Some(byte),
            None if byte == b'>' => return Some(i + 1),
            None => {}
        }
    }
    None
}

/// Like [`find_tag_end`] but also skips an internal subset in brackets, e.g., of a DOCTYPE.
fn find_declaration_end(declaration: &[u8]) -> Option<usize> {
    let mut quote = None;
    let mut depth = 0usize;
    for (i, &byte) in declaration.iter().enumerate() {
        match quote {
            Some(q) if byte == q => quote = None,
            Some(_) => {}
            None => match byte {
                b'"' | b'\'' => quote = Some(byte),
                b'[' => depth += 1,
                b']' => depth = depth.saturating_sub(1),
                b'>' if depth == 0 => return Some(i + 1),
                _ => {}
            },
        }
    }
    None
}

fn find_byte(haystack: &[u8], byte: u8) -> Option<usize> {
    // Checks 8 bytes at once. After the xor, the bytes equal to `byte` are zero. The expression
    // `(x - LO) & !x & HI` sets the highest bit of each zero byte. It can also set bits of bytes
    // following a zero byte because of the borrow, but never of bytes before the first zero byte.
    // So the lowest set bit marks the first match in little endian order.
    const LO: u64 = 0x0101_0101_0101_0101;
    const HI: u64 = 0x8080_8080_8080_8080;
    let pattern = LO * u64::from(byte);
    let mut chunks = haystack.chunks_exact(8);
    let mut offset = 0;
    for chunk in &mut chunks {
        let x = u64::from_le_bytes(chunk.try_into().unwrap()) ^ pattern;
        let found = x.wrapping_sub(LO) & !x & HI;
        if found != 0 {
            return Some(offset + (found.trailing_zeros() / 8) as usize);
        }
        offset += 8;
    }
    chunks
        .remainder()
        .iter()
        .position(|&b| b == byte)
        .map(|i| offset + i)
}

fn find_seq(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::{ElementSplitter, find_byte};
    use std::io::Read;

    #[test]
    fn find_byte_finds_first_match() {
        for len in 0..40 {
            for target in 0..=len {
                // All byte values around `<`, including those which would trip up the bit trick
                // if borrows were not accounted for.
                let haystack: Vec<u8> = (0..len)
                    .map(|i| {
                        if i == target {
                            b'<'
                        } else {
                            [0x3b, 0x3d, 0x00, 0xff, 0xbc][i % 5]
                        }
                    })
                    .collect();
                let expected = haystack.iter().position(|&b| b == b'<');
                assert_eq!(expected, find_byte(&haystack, b'<'), "{haystack:?}");
            }
        }
        assert_eq!(Some(1), find_byte(b"a<<<<<<<<<", b'<'));
    }

    /// Returns at most `chunk` bytes per read, so that markup is split across reads.
    struct SlowReader<'a> {
        data: &'a [u8],
        chunk: usize,
    }

    impl Read for SlowReader<'_> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = self.chunk.min(buf.len()).min(self.data.len());
            buf[..n].copy_from_slice(&self.data[..n]);
            self.data = &self.data[n..];
            Ok(n)
        }
    }

    fn split(xml: &str, chunk: usize) -> Vec<String> {
        let reader = SlowReader {
            data: xml.as_bytes(),
            chunk,
        };
        let mut splitter = ElementSplitter::new(reader, "person");
        let mut result = Vec::new();
        let mut out = Vec::new();
        while splitter.next_element_into(&mut out) {
            result.push(String::from_utf8(std::mem::take(&mut out)).unwrap());
        }
        result
    }

    fn assert_split(xml: &str, expected: &[&str]) {
        for chunk in [1, 2, 3, 7, 64, 1 << 20] {
            assert_eq!(expected, split(xml, chunk), "chunk size {chunk}");
        }
    }

    #[test]
    fn splits_persons_of_population() {
        let p1 =
            r#"<person id="1"><plan selected="yes"><activity type="h" link="1"/></plan></person>"#;
        let p2 = "<person\n\tid=\"2\" ><attributes><attribute name=\"a\" class=\"java.lang.String\">x</attribute></attributes></person >";
        let xml = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE population SYSTEM \"https://www.matsim.org/files/dtd/population_v6.dtd\">\n<population>\n<attributes><attribute name=\"coordinateReferenceSystem\" class=\"java.lang.String\">EPSG:25832</attribute></attributes>\n{p1}\n{p2}\n</population>\n"
        );
        assert_split(&xml, &[p1, p2]);
    }

    #[test]
    fn handles_self_closing_persons() {
        let p1 = r#"<person id="1"/>"#;
        let p2 = r#"<person id="2" />"#;
        let p3 = r#"<person id="3"></person>"#;
        assert_split(
            &format!("<population>{p1}{p2}{p3}</population>"),
            &[p1, p2, p3],
        );
    }

    #[test]
    fn ignores_markup_in_comments_cdata_and_declarations() {
        let p1 = r#"<person id="1"><!-- </person> --><attribute><![CDATA[</person><person id="x">]]></attribute></person>"#;
        let xml = format!(
            "<!DOCTYPE population [<!ENTITY p \"<person id='y'>\">]><population><!-- <person id=\"0\"></person> --><?pi <person>?>{p1}</population>"
        );
        assert_split(&xml, &[p1]);
    }

    #[test]
    fn handles_greater_than_in_attribute_values() {
        let p1 = r#"<person id="a>b" other='c/>'><activity type="x>y"/></person>"#;
        let p2 = r#"<person id="d/>"/>"#;
        assert_split(&format!("<population>{p1}{p2}</population>"), &[p1, p2]);
    }

    #[test]
    fn ignores_elements_with_longer_names() {
        let p1 = r#"<person id="1"><personAttributes/></person>"#;
        assert_split(
            &format!("<population><personX>a</personX><persons/>{p1}</population>"),
            &[p1],
        );
    }

    #[test]
    fn empty_population_has_no_persons() {
        assert_split("<population></population>", &[]);
        assert_split("<population/>", &[]);
    }

    #[test]
    fn matches_local_names_and_adds_namespace_declarations_of_root() {
        let xml = "<p:population xmlns:p=\"urn:a\" xmlns='urn:default' other=\"x\">\
                   <p:person id=\"1\"><p:plan/></p:person>\
                   <p:person xmlns:p=\"urn:own\" id=\"2\"/>\
                   </p:population>";
        assert_split(
            xml,
            &[
                "<p:person xmlns:p=\"urn:a\" xmlns=\"urn:default\" id=\"1\"><p:plan/></p:person>",
                // Elements which declare namespaces themselves are kept as they are.
                "<p:person xmlns:p=\"urn:own\" id=\"2\"/>",
            ],
        );
    }

    #[test]
    #[should_panic(expected = "Input ended before the end of the root element.")]
    fn input_ending_after_a_person_panics() {
        split("<population><person id=\"1\"/>\n", 1 << 20);
    }

    #[test]
    #[should_panic(expected = "Input ended before the end of the root element.")]
    fn input_ending_within_an_end_tag_panics() {
        split("<population><person id=\"1\"/></popu", 1 << 20);
    }

    #[test]
    #[should_panic(expected = "Input ended before the end of the root element.")]
    fn empty_input_panics() {
        split("", 1 << 20);
    }

    #[test]
    #[should_panic(expected = "Input ended within a <person> element.")]
    fn unclosed_person_panics() {
        split("<population><person id=\"1\"><plan>", 1 << 20);
    }

    #[test]
    #[should_panic(expected = "Input ended within a <person> element.")]
    fn unclosed_start_tag_panics() {
        split("<population><person id=\"1\"", 1 << 20);
    }

    #[test]
    #[should_panic(expected = "Nested <person> elements are not supported.")]
    fn nested_person_panics() {
        split("<person><person></person></person>", 1 << 20);
    }
}
