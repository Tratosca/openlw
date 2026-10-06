//! Lecture et écriture big-endian bornées, communes aux décodeurs.

use std::fmt;

/// Erreur de décodage ou d'encodage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Données plus courtes que nécessaire.
    Truncated {
        what: &'static str,
        need: usize,
        have: usize,
    },
    /// Valeur hors de ce que le format autorise.
    Invalid(&'static str),
    /// Valeur trop grande pour être encodée.
    TooLong(&'static str),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Truncated { what, need, have } => write!(
                f,
                "{what} tronqué : {need} octets requis, {have} disponibles"
            ),
            Error::Invalid(what) => write!(f, "valeur invalide : {what}"),
            Error::TooLong(what) => write!(f, "trop long : {what}"),
        }
    }
}

impl std::error::Error for Error {}

/// Curseur de lecture qui ne dépasse jamais la fin du tampon.
pub(crate) struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
    what: &'static str,
}

impl<'a> Reader<'a> {
    pub(crate) fn new(buf: &'a [u8], what: &'static str) -> Self {
        Self { buf, pos: 0, what }
    }

    pub(crate) fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    pub(crate) fn take(&mut self, n: usize) -> Result<&'a [u8], Error> {
        let end = self
            .pos
            .checked_add(n)
            .filter(|&e| e <= self.buf.len())
            .ok_or(Error::Truncated {
                what: self.what,
                need: self.pos.saturating_add(n),
                have: self.buf.len(),
            })?;
        let out = self
            .buf
            .get(self.pos..end)
            .ok_or(Error::Invalid("borne interne"))?;
        self.pos = end;
        Ok(out)
    }

    pub(crate) fn array<const N: usize>(&mut self) -> Result<[u8; N], Error> {
        let mut out = [0u8; N];
        out.copy_from_slice(self.take(N)?);
        Ok(out)
    }

    pub(crate) fn u8(&mut self) -> Result<u8, Error> {
        Ok(self.array::<1>()?[0])
    }

    pub(crate) fn u16(&mut self) -> Result<u16, Error> {
        Ok(u16::from_be_bytes(self.array()?))
    }

    pub(crate) fn u32(&mut self) -> Result<u32, Error> {
        Ok(u32::from_be_bytes(self.array()?))
    }

    pub(crate) fn u64(&mut self) -> Result<u64, Error> {
        Ok(u64::from_be_bytes(self.array()?))
    }

    pub(crate) fn rest(&mut self) -> &'a [u8] {
        let out = self.buf.get(self.pos..).unwrap_or_default();
        self.pos = self.buf.len();
        out
    }
}

/// Lit `N` octets à `offset`, sans paniquer.
pub(crate) fn at<const N: usize>(
    buf: &[u8],
    offset: usize,
    what: &'static str,
) -> Result<[u8; N], Error> {
    let mut r = Reader::new(buf, what);
    r.take(offset)?;
    r.array()
}
