//! What a consent question looks like, as pixels rather than as a window.
//!
//! # Why this is a crate of its own
//!
//! A host with a screen and no terminal could not ask anybody anything, so it
//! refused every session that needed a decision. That is the honest answer and
//! it is not a good one: a person who starts Esper from a desktop launcher or
//! as a user service has nowhere to say yes.
//!
//! The window, the keyboard grab and the event loop belong to a platform. The
//! *question* does not — where the words go, how a name that is too long is
//! cut, which pixels a click has to land on to mean yes — and all of that is
//! where a prompt gets a security decision wrong. So it lives here as a pure
//! function from a question to a picture, and it is checked by looking at the
//! pixels rather than by opening a display.
//!
//! # What this is not
//!
//! Not a widget toolkit and not the product's design language. Phase 10 is the
//! gate for how Esper looks, and this is deliberately plain until then: a
//! panel, a border, four kinds of text and two targets. What it must be is
//! legible and unambiguous, on every machine, without asking the machine for
//! anything.

pub mod font;

/// A rectangle of pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    /// Distance from the left edge.
    pub x: usize,
    /// Distance from the top edge.
    pub y: usize,
    /// Width in pixels.
    pub w: usize,
    /// Height in pixels.
    pub h: usize,
}

impl Rect {
    /// Whether a point is inside.
    ///
    /// Half-open on both axes, so two rectangles that touch cannot both hold
    /// the same pixel — the property that stops one answer's target from
    /// overlapping the other's.
    #[must_use]
    pub const fn holds(&self, x: usize, y: usize) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }

    /// Whether any pixel is in both.
    #[must_use]
    pub const fn overlaps(&self, other: &Self) -> bool {
        self.x < other.x + other.w
            && other.x < self.x + self.w
            && self.y < other.y + other.h
            && other.y < self.y + self.h
    }
}

/// A picture, one `0x00RR_GGBB` pixel per entry, row by row from the top.
///
/// The format is what X11's `ZPixmap` wants at 24 or 32 bits with no
/// conversion, which is the only reason it is 32 bits per pixel here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Canvas {
    /// Width in pixels.
    pub width: usize,
    /// Height in pixels.
    pub height: usize,
    /// `width * height` pixels.
    pub pixels: Vec<u32>,
}

impl Canvas {
    /// A canvas filled with one colour.
    #[must_use]
    pub fn new(width: usize, height: usize, fill: u32) -> Self {
        Self {
            width,
            height,
            pixels: vec![fill; width.saturating_mul(height)],
        }
    }

    /// Set one pixel, ignoring anything outside.
    ///
    /// Clipped rather than checked by every caller: a layout that computed a
    /// position one pixel off the edge should draw a slightly clipped prompt,
    /// not fail to draw one.
    pub fn set(&mut self, x: usize, y: usize, colour: u32) {
        if x >= self.width || y >= self.height {
            return;
        }
        if let Some(slot) = self.pixels.get_mut(y * self.width + x) {
            *slot = colour;
        }
    }

    /// What is at a pixel, or `None` outside.
    #[must_use]
    pub fn at(&self, x: usize, y: usize) -> Option<u32> {
        if x >= self.width || y >= self.height {
            return None;
        }
        self.pixels.get(y * self.width + x).copied()
    }

    /// Fill a rectangle.
    pub fn fill(&mut self, r: Rect, colour: u32) {
        for y in r.y..r.y.saturating_add(r.h) {
            for x in r.x..r.x.saturating_add(r.w) {
                self.set(x, y, colour);
            }
        }
    }

    /// Draw a rectangle's outline, `thick` pixels inside its own bounds.
    pub fn outline(&mut self, r: Rect, thick: usize, colour: u32) {
        for i in 0..thick {
            let inset = Rect {
                x: r.x.saturating_add(i),
                y: r.y.saturating_add(i),
                w: r.w.saturating_sub(i * 2),
                h: r.h.saturating_sub(i * 2),
            };
            if inset.w == 0 || inset.h == 0 {
                return;
            }
            for x in inset.x..inset.x + inset.w {
                self.set(x, inset.y, colour);
                self.set(x, inset.y + inset.h - 1, colour);
            }
            for y in inset.y..inset.y + inset.h {
                self.set(inset.x, y, colour);
                self.set(inset.x + inset.w - 1, y, colour);
            }
        }
    }

    /// Draw one character, its top-left at `x, y`, every pixel `scale` square.
    pub fn glyph(&mut self, c: char, x: usize, y: usize, scale: usize, colour: u32) {
        let rows = font::glyph(c);
        for (row, bits) in rows.iter().enumerate() {
            for column in 0..font::GLYPH_WIDTH {
                // Bit four is the leftmost pixel, so a row of the font reads
                // the way it is written.
                let shift = font::GLYPH_WIDTH - 1 - column;
                if bits & (1 << shift) == 0 {
                    continue;
                }
                for dy in 0..scale {
                    for dx in 0..scale {
                        self.set(x + column * scale + dx, y + row * scale + dy, colour);
                    }
                }
            }
        }
    }

    /// Draw a line of text, its top-left at `x, y`.
    pub fn text(&mut self, s: &str, x: usize, y: usize, scale: usize, colour: u32) {
        for (i, c) in s.chars().enumerate() {
            self.glyph(c, x + i * font::ADVANCE * scale, y, scale, colour);
        }
    }

    /// How many pixels are not `background`.
    ///
    /// The measurement the tests are written against: a layout is checked by
    /// asking where there is ink, which is the same question a person asks
    /// looking at it.
    #[must_use]
    pub fn ink(&self, background: u32) -> usize {
        self.pixels.iter().filter(|p| **p != background).count()
    }

    /// How many pixels inside a rectangle are not `background`.
    #[must_use]
    pub fn ink_in(&self, r: Rect, background: u32) -> usize {
        let mut seen = 0;
        for y in r.y..r.y.saturating_add(r.h) {
            for x in r.x..r.x.saturating_add(r.w) {
                if self.at(x, y).is_some_and(|p| p != background) {
                    seen += 1;
                }
            }
        }
        seen
    }
}

/// What somebody is being asked.
#[derive(Debug, Clone)]
pub struct Question {
    /// What kind of session: a screen, a shell, a forward.
    pub kind: String,
    /// What the device is called. **Peer-supplied**, so it is cut to fit and
    /// anything this font cannot draw is drawn as a box that says so.
    pub name: String,
    /// The key's word form, which is the identity somebody compares. Always
    /// drawable: it comes from this project's own word list.
    pub fingerprint: String,
    /// What the session would be able to do, already clamped to what will
    /// happen rather than what was asked for.
    pub granted: Vec<String>,
    /// How long is left to answer, in seconds.
    pub seconds: u64,
}

/// A painted question, and where its two answers are.
#[derive(Debug, Clone)]
pub struct Painted {
    /// The picture.
    pub canvas: Canvas,
    /// Where a click means yes.
    pub allow: Rect,
    /// Where a click means no.
    pub refuse: Rect,
}

/// The colours, in one place.
///
/// Plain on purpose — see the note at the top of this file. The border is the
/// one loud thing, because a prompt that appears has to be noticed.
mod ink {
    /// Behind everything.
    pub const PANEL: u32 = 0x0016_181c;
    /// The border, and the one loud colour here.
    pub const EDGE: u32 = 0x00e4_a72e;
    /// Ordinary text.
    pub const TEXT: u32 = 0x00f2_f4f8;
    /// Labels, and anything that is context rather than content.
    pub const DIM: u32 = 0x0092_9aa6;
    /// The identity somebody compares, which is the most important text here.
    pub const IDENTITY: u32 = 0x0079_d0f0;
    /// The refusing answer, which is the one a stray click should land on.
    pub const REFUSE: u32 = 0x0033_3a44;
    /// The allowing answer, outlined rather than filled.
    pub const ALLOW: u32 = 0x0064_c07a;
}

/// How much bigger than the font's own pixels the body text is drawn.
const BODY: usize = 2;

/// And the title, which is the first thing read.
const TITLE: usize = 3;

/// How wide every question is, in characters of body text.
///
/// There is no terminal to take a width from, so this *is* the width — see
/// [`the_width`]. Forty-four fits the longest line this layout can produce
/// with a little slack: the heading is at most `a device wants to open a `
/// plus a session kind plus ` session`, the fingerprint's four-word halves sit
/// under a ten-character label, and a name is cut to [`NAME_SHOWN`] so it
/// cannot be the thing that overflows.
const COLUMNS: usize = 44;

/// The most of a peer-supplied name to draw.
///
/// Shorter than the column limit so a name cannot be the widest thing in the
/// window: the label in front of it takes room, and a name that set the
/// window's width could push the identity off somebody's screen.
const NAME_SHOWN: usize = 32;

/// Space between the border and the content.
const PADDING: usize = 22;

/// Space between one line of text and the next.
const LEADING: usize = 8;

/// Space above and below a group of lines that belong together.
const GAP: usize = 16;

/// One line of the question's body, and how it is drawn.
///
/// **The lines are a list rather than a sequence of statements**, because the
/// height has to be worked out from the same thing that draws them. The first
/// version computed the height from a count and then drew the lines one after
/// another, the count was one short, and the last line was drawn over the
/// bottom border — which the border test caught, and which no amount of care
/// with the arithmetic would have prevented from happening again.
struct Line {
    text: String,
    /// How far in from the left margin, for the fingerprint under its label.
    indent: usize,
    colour: u32,
}

/// Paint the question.
///
/// The size follows the content: a short name gives a small window. What does
/// **not** follow the content is the maximum — see [`COLUMNS`] — because a
/// peer-supplied name must not be able to decide how big a window on somebody
/// else's screen is.
#[must_use]
pub fn paint(q: &Question) -> Painted {
    let line_height = font::GLYPH_HEIGHT * BODY;
    let title_height = font::GLYPH_HEIGHT * TITLE;

    let mut body = vec![
        Line {
            text: format!("a device wants to open a {} session", cut(&q.kind, 10)),
            indent: 0,
            colour: ink::TEXT,
        },
        Line {
            text: format!("from      {}", cut(&q.name, NAME_SHOWN)),
            indent: 0,
            colour: ink::TEXT,
        },
    ];
    // **The identity, in halves.** Eight words in one line is read as a
    // sentence and skimmed; four and four is read as a code and checked, which
    // is the entire reason the word form exists — so this splits on words
    // rather than on how much happens to fit.
    for half in halves(&q.fingerprint) {
        body.push(Line {
            text: half,
            indent: indent(),
            colour: ink::IDENTITY,
        });
    }
    let what = if q.granted.is_empty() {
        "nothing".to_owned()
    } else {
        q.granted.join(", ")
    };
    for (i, line) in wrap(&format!("allowing  {what}"), COLUMNS)
        .into_iter()
        .enumerate()
    {
        body.push(Line {
            text: line,
            indent: if i == 0 { 0 } else { indent() },
            colour: ink::DIM,
        });
    }
    let footer = format!("nobody answering means no  {}s", q.seconds.clamp(1, 9999));

    let button_height = line_height + PADDING;
    let button_width = font::width_of("Refuse", BODY) + PADDING * 2;
    let buttons_width = button_width * 2 + GAP;

    // **The width is fixed, not fitted.** Fitting it to the content was
    // tried and it lets the far end change the shape of the window: a name is
    // peer-supplied, and even cut to a bound it made the prompt 546 pixels
    // wide where a short name made it 510.
    //
    // A prompt is the one thing here that somebody learns the shape of, and a
    // prompt that is a different shape each time teaches nobody anything. So
    // every question on a machine is the same width, and the content is laid
    // out inside it.
    let width = the_width();
    debug_assert!(
        buttons_width + PADDING * 2 <= width,
        "the answers do not fit the window"
    );

    let height = PADDING
        + title_height
        + GAP
        + body.len() * line_height
        + body.len().saturating_sub(1) * LEADING
        + GAP
        + GAP
        + button_height
        + GAP
        + line_height
        + PADDING;

    let mut canvas = Canvas::new(width, height, ink::PANEL);
    let left = PADDING;
    let mut y = PADDING;
    canvas.text("Esper", left, y, TITLE, ink::TEXT);
    y += title_height + GAP;

    for (i, line) in body.iter().enumerate() {
        if i > 0 {
            y += line_height + LEADING;
        }
        canvas.text(&line.text, left + line.indent, y, BODY, line.colour);
    }
    y += line_height + GAP + GAP;

    // **Refusing is in the corner a reflex reaches for.**
    //
    // Deliberately the other way round from most dialogs, which put the
    // affirmative action at the bottom right. Somebody dismissing this
    // without reading it is the failure worth designing against, and the two
    // outcomes are not symmetrical: a wrong refusal costs a reconnect, and a
    // wrong allowance puts a stranger on somebody's screen.
    //
    // Anybody who reads the prompt hits the button they meant either way. It
    // is the person who does not read it whose habit lands somewhere, and this
    // is the somewhere it should land — the same argument as `[y/N]` on the
    // terminal, where the default is also no.
    let right = width - PADDING;
    let refuse = Rect {
        x: right - button_width,
        y,
        w: button_width,
        h: button_height,
    };
    let allow = Rect {
        x: right - button_width * 2 - GAP,
        y,
        w: button_width,
        h: button_height,
    };
    canvas.fill(refuse, ink::REFUSE);
    canvas.text(
        "Refuse",
        centre(refuse.x, refuse.w, "Refuse"),
        y + PADDING / 2,
        BODY,
        ink::TEXT,
    );
    canvas.outline(allow, 2, ink::ALLOW);
    canvas.text(
        "Allow",
        centre(allow.x, allow.w, "Allow"),
        y + PADDING / 2,
        BODY,
        ink::ALLOW,
    );
    y += button_height + GAP;
    canvas.text(&footer, left, y, BODY, ink::DIM);

    // **Last, so it cannot be drawn over.** The border is the evidence that
    // nothing was laid out past the window: a hole in it means a line
    // overran, and a line that overran is text somebody cannot read.
    canvas.outline(
        Rect {
            x: 0,
            y: 0,
            w: width,
            h: height,
        },
        3,
        ink::EDGE,
    );

    Painted {
        canvas,
        allow,
        refuse,
    }
}

/// How wide every question is, in pixels.
///
/// A column count rather than a pixel count, so the font's own size is what
/// decides it and the two cannot drift.
#[must_use]
pub fn the_width() -> usize {
    font::width_of(&"n".repeat(COLUMNS), BODY) + PADDING * 2
}

/// The fingerprint in halves of four words.
///
/// Whatever arrives: a hex string with no spaces comes back as one line, which
/// is not the word form and is still an identity somebody can compare.
fn halves(fingerprint: &str) -> Vec<String> {
    let words: Vec<&str> = fingerprint.split_whitespace().collect();
    if words.is_empty() {
        return vec![String::new()];
    }
    let per = words.len().div_ceil(2).max(1);
    words
        .chunks(per)
        .map(|chunk| cut(&chunk.join(" "), COLUMNS))
        .collect()
}

/// How far the fingerprint is indented from the labels above it.
fn indent() -> usize {
    font::width_of("from      ", BODY)
}

/// Where text starts so that it sits in the middle of a button.
fn centre(x: usize, w: usize, text: &str) -> usize {
    x + w.saturating_sub(font::width_of(text, BODY)) / 2
}

/// Cut `text` to `most` characters, marking that it was cut.
///
/// Counted in characters rather than bytes, and the marker is a character this
/// font has: an ellipsis would be drawn as the unknown box, which reads as a
/// name containing something strange rather than as a name that was shortened.
fn cut(text: &str, most: usize) -> String {
    if text.chars().count() <= most {
        return text.to_owned();
    }
    let kept: String = text.chars().take(most.saturating_sub(3)).collect();
    format!("{kept}...")
}

/// Break `text` into lines of at most `most` characters, on spaces.
///
/// A word longer than a line is cut rather than pushed past the edge, because
/// the one string here that a peer chose is the one that would be.
fn wrap(text: &str, most: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let word = cut(word, most);
        let would_be = if line.is_empty() {
            word.chars().count()
        } else {
            line.chars().count() + 1 + word.chars().count()
        };
        if would_be > most && !line.is_empty() {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(&word);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn question() -> Question {
        Question {
            kind: "screen".to_owned(),
            name: "elythion".to_owned(),
            fingerprint: "giraffe tapestry geyser rifle jersey domain razor oboe".to_owned(),
            granted: vec!["view".to_owned(), "control".to_owned()],
            seconds: 45,
        }
    }

    /// Every character this font claims to cover is actually drawn.
    ///
    /// A glyph left empty by a typo in the art would be a silent hole: a name
    /// with an `S` in it would read with a gap, and two different names could
    /// look identical.
    #[test]
    fn every_printable_character_has_a_glyph() {
        for c in ' '..='~' {
            let g = font::glyph(c);
            assert!(
                font::drawable(c),
                "{c:?} is inside the range and not drawable"
            );
            if c == ' ' {
                assert_eq!(g, [0; font::GLYPH_HEIGHT], "space is not blank");
                continue;
            }
            assert_ne!(g, [0; font::GLYPH_HEIGHT], "{c:?} has no ink");
            assert_ne!(g, font::UNKNOWN, "{c:?} is drawn as the unknown box");
        }
    }

    /// And a character it cannot draw looks like one.
    ///
    /// A peer chooses the device name. A silent gap would let a name with
    /// something unrenderable in it read as a shorter, different name — and
    /// the whole point of showing the name is that somebody recognises it.
    #[test]
    fn a_character_this_font_cannot_draw_is_visibly_not_drawn() {
        assert_eq!(font::glyph('é'), font::UNKNOWN);
        assert_eq!(font::glyph('\u{1f600}'), font::UNKNOWN);

        let mut with = Canvas::new(40, 20, 0);
        with.text("é", 0, 0, 1, 1);
        let mut without = Canvas::new(40, 20, 0);
        without.text(" ", 0, 0, 1, 1);
        assert!(
            with.ink(0) > without.ink(0),
            "an undrawable character has to leave a mark"
        );
    }

    /// The two answers are inside the picture and share no pixel.
    ///
    /// The property a click depends on: a point that is in both rectangles is
    /// a click that means two things, and one of them is yes.
    #[test]
    fn the_two_answers_share_no_pixel() {
        let p = paint(&question());
        assert!(!p.allow.overlaps(&p.refuse), "{:?} {:?}", p.allow, p.refuse);
        for r in [p.allow, p.refuse] {
            assert!(r.x + r.w <= p.canvas.width, "{r:?} is off the right edge");
            assert!(r.y + r.h <= p.canvas.height, "{r:?} is off the bottom");
            assert!(r.w > 0 && r.h > 0, "{r:?} is a target nobody can hit");
        }
        assert!(
            p.allow
                .holds(p.allow.x + p.allow.w / 2, p.allow.y + p.allow.h / 2)
        );
        assert!(
            !p.refuse
                .holds(p.allow.x + p.allow.w / 2, p.allow.y + p.allow.h / 2)
        );
    }

    /// A peer-supplied name cannot decide how big a window on somebody else's
    /// screen is.
    ///
    /// Left alone, the widest line sets the width — which would hand the far
    /// end a window as wide as it liked, with the identity pushed off the
    /// screen it was meant to be compared on.
    #[test]
    fn no_name_can_decide_how_big_the_window_is() {
        let ordinary = paint(&question());
        assert_eq!(ordinary.canvas.width, the_width());
        for name in ["", "elythion", &"n".repeat(4_000), "é\u{1f600}\u{1f600}"] {
            let mut q = question();
            q.name = name.to_owned();
            let painted = paint(&q);
            assert_eq!(
                painted.canvas.width, ordinary.canvas.width,
                "the name {name:?} changed the window's width"
            );
            assert_eq!(
                painted.canvas.height, ordinary.canvas.height,
                "the name {name:?} changed the window's height"
            );
        }
    }

    /// And the identity is drawn whatever the name is.
    ///
    /// The name is a convenience and the fingerprint is the identity. A prompt
    /// that showed one and not the other would be asking somebody to trust a
    /// string the far end chose.
    #[test]
    fn the_identity_is_always_drawn() {
        for name in ["elythion", "", &"n".repeat(4_000), "é\u{1f600}"] {
            let mut q = question();
            q.name = name.to_owned();
            let p = paint(&q);
            let identity = Rect {
                x: 0,
                y: 0,
                w: p.canvas.width,
                h: p.refuse.y,
            };
            let blue = p.canvas.ink_in(identity, ink::PANEL);
            assert!(
                blue > 200,
                "the identity is missing for {name:?}: {blue} ink"
            );
        }
    }

    /// The fingerprint's halves each fit on a line of their own.
    ///
    /// Eight words is read as a sentence and skimmed; four and four is read as
    /// a code and checked — which is the whole reason the word form exists.
    #[test]
    fn the_fingerprint_is_shown_in_halves_rather_than_as_a_sentence() {
        let lines = halves(&question().fingerprint);
        assert_eq!(lines.len(), 2, "{lines:?}");
        for line in &lines {
            assert_eq!(line.split_whitespace().count(), 4, "{lines:?}");
        }
        // A hex fingerprint has no words to split, and is still an identity.
        let hex = halves("be41344cf00e55422005b232a03d3a0d");
        assert_eq!(hex.len(), 1, "{hex:?}");
        assert_eq!(halves(""), vec![String::new()]);
    }

    /// Wrapping breaks on spaces, and a word too long for a line is cut.
    #[test]
    fn wrapping_breaks_on_spaces_and_cuts_what_cannot_fit() {
        assert_eq!(wrap("one two three", 7), vec!["one two", "three"]);
        let long = wrap(&"x".repeat(50), 10);
        assert_eq!(long.len(), 1);
        let only = long.first().expect("one line");
        assert_eq!(only.chars().count(), 10);
        assert!(only.ends_with("..."), "{long:?}");
        assert_eq!(wrap("", 10), vec![String::new()]);
    }

    /// A cut name says it was cut, in a character this font has.
    #[test]
    fn a_cut_name_says_so_drawably() {
        let cut = cut(&"a".repeat(100), 10);
        assert_eq!(cut.chars().count(), 10);
        assert!(cut.ends_with("..."));
        for c in cut.chars() {
            assert!(font::drawable(c), "{c:?} in a cut marker is not drawable");
        }
    }

    /// The border survives the content.
    ///
    /// Text that overran would draw over the edge, and an edge with a hole in
    /// it is the visible sign that something was laid out past the window.
    #[test]
    fn nothing_is_drawn_over_the_border() {
        let mut q = question();
        q.name = "n".repeat(4_000);
        q.granted = vec!["view".to_owned(); 12];
        let p = paint(&q);
        for y in 0..p.canvas.height {
            for x in [
                0,
                1,
                2,
                p.canvas.width - 3,
                p.canvas.width - 2,
                p.canvas.width - 1,
            ] {
                assert_eq!(p.canvas.at(x, y), Some(ink::EDGE), "at {x},{y}");
            }
        }
        for x in 0..p.canvas.width {
            for y in [
                0,
                1,
                2,
                p.canvas.height - 3,
                p.canvas.height - 2,
                p.canvas.height - 1,
            ] {
                assert_eq!(p.canvas.at(x, y), Some(ink::EDGE), "at {x},{y}");
            }
        }
    }

    /// A session that would be allowed nothing says so rather than going blank.
    #[test]
    fn a_question_with_nothing_granted_still_says_what_it_means() {
        let mut q = question();
        q.granted.clear();
        let p = paint(&q);
        assert!(p.canvas.ink(ink::PANEL) > 1_000);
    }

    /// Look at the whole prompt, at the size somebody would see it.
    ///
    /// The assertions above are about ink in the right places, and none of
    /// them can tell a legible prompt from a jumble. This is how the layout
    /// was checked, and it is how the next change to it should be.
    #[test]
    #[ignore = "prints the prompt; run deliberately with --ignored --nocapture"]
    fn look_at_the_prompt() {
        let p = paint(&question());
        println!("{}x{}", p.canvas.width, p.canvas.height);
        // Two pixels per character across and four down, so the aspect is
        // roughly right in a terminal.
        for y in (0..p.canvas.height).step_by(4) {
            let row: String = (0..p.canvas.width)
                .step_by(2)
                .map(|x| match p.canvas.at(x, y) {
                    Some(ink::PANEL) => ' ',
                    Some(ink::EDGE) => '=',
                    Some(ink::IDENTITY) => 'o',
                    Some(ink::DIM) => '-',
                    Some(ink::ALLOW) => '+',
                    Some(ink::REFUSE) => ':',
                    _ => '#',
                })
                .collect();
            println!("{row}");
        }
    }

    /// Print the whole font for somebody to look at.
    ///
    /// **A font nobody has read is a font nobody has checked.** Every
    /// assertion below is about ink in the right places, and none of them can
    /// tell an `S` from a `5` — so this exists to be run by hand, and it is
    /// how the glyphs were proof-read.
    #[test]
    #[ignore = "prints the font; run deliberately with --ignored --nocapture"]
    fn look_at_the_font() {
        let lines = [
            "ABCDEFGHIJKLM",
            "NOPQRSTUVWXYZ",
            "abcdefghijklm",
            "nopqrstuvwxyz",
            "0123456789 !?",
            "The quick brown",
            "fox jumps over",
            "the lazy dog.",
            "@#$%&*()[]{}<>",
            "+-=_/\\|~^'\"`,;:",
        ];
        for line in lines {
            let mut c = Canvas::new(font::width_of(line, 1), font::GLYPH_HEIGHT, 0);
            c.text(line, 0, 0, 1, 1);
            for y in 0..c.height {
                let row: String = (0..c.width)
                    .map(|x| if c.at(x, y) == Some(1) { '#' } else { '.' })
                    .collect();
                println!("{row}");
            }
            println!();
        }
    }
}
