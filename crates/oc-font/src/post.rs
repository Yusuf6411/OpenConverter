//! Glyph names from the `post` table.
use crate::reader::R;

/// The 258 glyph names of the Macintosh standard ordering used by `post` format 1 and 2.
pub const MAC_GLYPHS: [&str; 258] = [
    ".notdef", ".null", "nonmarkingreturn", "space", "exclam", "quotedbl", "numbersign", "dollar",
    "percent", "ampersand", "quotesingle", "parenleft", "parenright", "asterisk", "plus", "comma",
    "hyphen", "period", "slash", "zero", "one", "two", "three", "four", "five", "six", "seven",
    "eight", "nine", "colon", "semicolon", "less", "equal", "greater", "question", "at", "A", "B",
    "C", "D", "E", "F", "G", "H", "I", "J", "K", "L", "M", "N", "O", "P", "Q", "R", "S", "T", "U",
    "V", "W", "X", "Y", "Z", "bracketleft", "backslash", "bracketright", "asciicircum",
    "underscore", "grave", "a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l", "m", "n",
    "o", "p", "q", "r", "s", "t", "u", "v", "w", "x", "y", "z", "braceleft", "bar", "braceright",
    "asciitilde", "Adieresis", "Aring", "Ccedilla", "Eacute", "Ntilde", "Odieresis", "Udieresis",
    "aacute", "agrave", "acircumflex", "adieresis", "atilde", "aring", "ccedilla", "eacute",
    "egrave", "ecircumflex", "edieresis", "iacute", "igrave", "icircumflex", "idieresis", "ntilde",
    "oacute", "ograve", "ocircumflex", "odieresis", "otilde", "uacute", "ugrave", "ucircumflex",
    "udieresis", "dagger", "degree", "cent", "sterling", "section", "bullet", "paragraph",
    "germandbls", "registered", "copyright", "trademark", "acute", "dieresis", "notequal", "AE",
    "Oslash", "infinity", "plusminus", "lessequal", "greaterequal", "yen", "mu", "partialdiff",
    "summation", "product", "pi", "integral", "ordfeminine", "ordmasculine", "Omega", "ae",
    "oslash", "questiondown", "exclamdown", "logicalnot", "radical", "florin", "approxequal",
    "Delta", "guillemotleft", "guillemotright", "ellipsis", "nonbreakingspace", "Agrave", "Atilde",
    "Otilde", "OE", "oe", "endash", "emdash", "quotedblleft", "quotedblright", "quoteleft",
    "quoteright", "divide", "lozenge", "ydieresis", "Ydieresis", "fraction", "currency",
    "guilsinglleft", "guilsinglright", "fi", "fl", "daggerdbl", "periodcentered", "quotesinglbase",
    "quotedblbase", "perthousand", "Acircumflex", "Ecircumflex", "Aacute", "Edieresis", "Egrave",
    "Iacute", "Icircumflex", "Idieresis", "Igrave", "Oacute", "Ocircumflex", "apple", "Ograve",
    "Uacute", "Ucircumflex", "Ugrave", "dotlessi", "circumflex", "tilde", "macron", "breve",
    "dotaccent", "ring", "cedilla", "hungarumlaut", "ogonek", "caron", "Lslash", "lslash",
    "Scaron", "scaron", "Zcaron", "zcaron", "brokenbar", "Eth", "eth", "Yacute", "yacute",
    "Thorn", "thorn", "minus", "multiply", "onesuperior", "twosuperior", "threesuperior",
    "onehalf", "onequarter", "threequarters", "franc", "Gbreve", "gbreve", "Idotaccent",
    "Scedilla", "scedilla", "Cacute", "cacute", "Ccaron", "ccaron", "dcroat",
];

pub fn glyph_name(post: &[u8], gid: u16, num_glyphs: u16) -> Option<String> {
    let r = R::new(post);
    match r.u32(0)? {
        0x0001_0000 => MAC_GLYPHS.get(gid as usize).map(|s| (*s).to_string()),
        0x0002_0000 => {
            let n = r.u16(32)? as usize;
            if gid as usize >= n {
                return None;
            }
            let idx = r.u16(34 + gid as usize * 2)? as usize;
            if idx < 258 {
                return MAC_GLYPHS.get(idx).map(|s| (*s).to_string());
            }
            // Pascal strings follow the index array.
            let mut o = 34 + n * 2;
            let want = idx - 258;
            for i in 0..=want {
                let len = r.u8(o)? as usize;
                if i == want {
                    let b = r.slice(o + 1, len)?;
                    return Some(b.iter().map(|c| char::from(*c)).collect());
                }
                o += 1 + len;
            }
            None
        }
        _ => {
            let _ = num_glyphs;
            None
        }
    }
}
