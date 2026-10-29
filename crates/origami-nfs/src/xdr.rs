//! XDR (RFC 4506) decoding of a request body and encoding of a reply.

/// Decodes values from a request. A short or malformed body sets `err`,
/// which the dispatcher turns into GARBAGE_ARGS; the values read after
/// that are zero.
pub(crate) struct Reader<'a> {
    pub b: &'a [u8],
    pub off: usize,
    pub err: bool,
}

impl<'a> Reader<'a> {
    pub fn new(b: &'a [u8]) -> Reader<'a> {
        Reader {
            b,
            off: 0,
            err: false,
        }
    }

    pub fn u32(&mut self) -> u32 {
        if self.off + 4 > self.b.len() {
            self.err = true;
            return 0;
        }
        let v = u32::from_be_bytes(self.b[self.off..self.off + 4].try_into().unwrap());
        self.off += 4;
        v
    }

    pub fn u64(&mut self) -> u64 {
        let hi = self.u32() as u64;
        hi << 32 | self.u32() as u64
    }

    pub fn bool(&mut self) -> bool {
        self.u32() != 0
    }

    /// A fixed-length opaque of exactly `n` bytes (no length prefix, no
    /// padding), such as the 32-byte NFSv2 filehandle.
    pub fn bytes(&mut self, n: usize) -> &'a [u8] {
        if self.off + n > self.b.len() {
            self.err = true;
            return &[];
        }
        let v = &self.b[self.off..self.off + n];
        self.off += n;
        v
    }

    /// A length-prefixed, 4-byte-padded opaque of at most `max` bytes.
    pub fn opaque(&mut self, max: usize) -> &'a [u8] {
        let n = self.u32() as usize;
        if self.err || n > max || self.off + n > self.b.len() {
            self.err = true;
            return &[];
        }
        let v = &self.b[self.off..self.off + n];
        self.off += (n + 3) & !3;
        if self.off > self.b.len() {
            self.err = true;
        }
        v
    }

    /// A length-prefixed, 4-byte-padded string.
    pub fn str(&mut self) -> &'a [u8] {
        self.opaque(self.b.len())
    }
}

/// Encodes values into a reply body.
#[derive(Default)]
pub(crate) struct Writer {
    pub b: Vec<u8>,
}

impl Writer {
    pub fn new() -> Writer {
        Writer::default()
    }

    pub fn u32(&mut self, v: u32) {
        self.b.extend_from_slice(&v.to_be_bytes());
    }

    pub fn u64(&mut self, v: u64) {
        self.b.extend_from_slice(&v.to_be_bytes());
    }

    pub fn bool(&mut self, v: bool) {
        self.u32(v as u32);
    }

    /// Raw bytes with no length prefix or padding.
    pub fn fixed(&mut self, p: &[u8]) {
        self.b.extend_from_slice(p);
    }

    /// A length-prefixed, 4-byte-padded byte string.
    pub fn opaque(&mut self, p: &[u8]) {
        self.u32(p.len() as u32);
        self.b.extend_from_slice(p);
        while !self.b.len().is_multiple_of(4) {
            self.b.push(0);
        }
    }
}
