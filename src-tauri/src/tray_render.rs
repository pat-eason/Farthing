//! Native stacked menu-bar budget readout (macOS only).
//!
//! Tauri's `set_title` only takes a single plain line, so the stacked layout
//! the budgets feature wants —
//!
//! ```text
//! (bird) $123 | D 34% ●
//!             | M 78% ●
//! ```
//!
//! a bird glyph, a large vertically-centred cost, a full-height pipe, then one
//! row per set budget with a trailing stoplight dot — is drawn into a PNG and
//! installed through Tauri's `set_icon` (NOT a raw `setImage`: `set_icon` keeps
//! tray-icon's click-target overlay sized to the button, so left-click still
//! toggles the popover instead of falling through to the menu).
//!
//! Drawn into a 3x [`NSBitmapImageRep`] (crisp on retina once tray-icon scales
//! it to the 18pt menu-bar height) with a transparent background and text
//! colour chosen from the effective appearance (legible on light or dark bars).

use core::ffi::c_uchar;

use objc2::rc::Retained;
use objc2::{AnyThread, MainThreadMarker};
use objc2_app_kit::{
    NSApplication, NSAttributedStringNSStringDrawing, NSBezierPath, NSBitmapFormat,
    NSBitmapImageFileType, NSBitmapImageRep, NSColor, NSCompositingOperation,
    NSDeviceRGBColorSpace, NSFont, NSFontAttributeName, NSForegroundColorAttributeName,
    NSGraphicsContext, NSImage,
};
use objc2_foundation::{
    NSDictionary, NSMutableAttributedString, NSPoint, NSRange, NSRect, NSSize, NSString,
};

use crate::budgets::Band;

/// The bird glyph (template silhouette), drawn tinted at the left.
const BIRD_PNG: &[u8] = include_bytes!("../icons/tray-icon.png");

/// One budget row in the readout (e.g. `D` / `34%` / amber).
#[derive(Debug, Clone)]
pub struct Row {
    /// Single-letter budget marker: `D` (daily) or `M` (monthly).
    pub marker: &'static str,
    pub percent: i64,
    pub band: Band,
}

/// What the menu-bar widget should show: today's cost plus 0-2 budget rows.
#[derive(Debug, Clone)]
pub struct Model {
    pub cost: String,
    pub rows: Vec<Row>,
}

// ---- layout (logical points; tray-icon scales the result to 18pt tall) ----
const SCALE: f64 = 3.0;
const HEIGHT: f64 = 18.0;
const PAD: f64 = 2.0;
const BIRD_W: f64 = 17.0; // 82:64 aspect at ~13pt tall
const BIRD_H: f64 = 13.0;
const GAP_BIRD_COST: f64 = 4.0;
const GAP_COST_PIPE: f64 = 5.0;
const GAP_PIPE_ROWS: f64 = 6.0;
const GAP_TEXT_DOT: f64 = 4.0;
const DOT: f64 = 5.0;
const ROW_H: f64 = 9.0;
const COST_FONT: f64 = 12.0;
const ROW_FONT: f64 = 8.5;

fn px(points: f64) -> f64 {
    points * SCALE
}

fn dot_color(band: Band) -> Retained<NSColor> {
    match band {
        Band::Green => NSColor::systemGreenColor(),
        Band::Yellow => NSColor::systemYellowColor(),
        Band::Amber => NSColor::systemOrangeColor(),
        Band::Red => NSColor::systemRedColor(),
    }
}

/// Is the effective (menu-bar) appearance dark? Picks legible text colour.
fn is_dark(mtm: MainThreadMarker) -> bool {
    let app = NSApplication::sharedApplication(mtm);
    app.effectiveAppearance()
        .name()
        .to_string()
        .contains("Dark")
}

/// Build an attributed string carrying `text` in `font` and `color`.
fn attributed(text: &str, font: &NSFont, color: &NSColor) -> Retained<NSMutableAttributedString> {
    let s = NSMutableAttributedString::initWithString(
        NSMutableAttributedString::alloc(),
        &NSString::from_str(text),
    );
    let range = NSRange {
        location: 0,
        length: s.length(),
    };
    unsafe {
        s.addAttribute_value_range(NSFontAttributeName, font, range);
        s.addAttribute_value_range(NSForegroundColorAttributeName, color, range);
    }
    s
}

/// Render the widget to PNG bytes. `None` on any allocation/encoding failure
/// (caller falls back to a plain title). Must run on the main thread.
pub fn render_png(model: &Model, mtm: MainThreadMarker) -> Option<Vec<u8>> {
    let dark = is_dark(mtm);
    let text_color = if dark {
        NSColor::whiteColor()
    } else {
        NSColor::blackColor()
    };
    let pipe_color = text_color.colorWithAlphaComponent(0.35);

    let cost_font = NSFont::boldSystemFontOfSize(px(COST_FONT));
    let row_font = NSFont::systemFontOfSize(px(ROW_FONT));

    // Segments (measured in the same scaled space they're drawn in).
    let cost = attributed(&model.cost, &cost_font, &text_color);
    let cost_size: NSSize = cost.size();
    let rows: Vec<(Retained<NSMutableAttributedString>, NSSize, Band)> = model
        .rows
        .iter()
        .map(|r| {
            let a = attributed(
                &format!("{} {}%", r.marker, r.percent),
                &row_font,
                &text_color,
            );
            let size = a.size();
            (a, size, r.band)
        })
        .collect();

    let h_px = px(HEIGHT);
    let rows_text_w = rows.iter().map(|(_, s, _)| s.width).fold(0.0_f64, f64::max);
    let rows_w = if rows.is_empty() {
        0.0
    } else {
        rows_text_w + px(GAP_TEXT_DOT) + px(DOT)
    };

    let bird_x = px(PAD);
    let cost_x = bird_x + px(BIRD_W) + px(GAP_BIRD_COST);
    let pipe_x = cost_x + cost_size.width + px(GAP_COST_PIPE);
    let rows_x = pipe_x + px(1.0) + px(GAP_PIPE_ROWS);
    let w_px = if rows.is_empty() {
        cost_x + cost_size.width + px(PAD)
    } else {
        rows_x + rows_w + px(PAD)
    };

    let width = w_px.ceil() as isize;
    let height = h_px.ceil() as isize;

    let rep = unsafe {
        NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bitmapFormat_bytesPerRow_bitsPerPixel(
            NSBitmapImageRep::alloc(),
            core::ptr::null_mut::<*mut c_uchar>(),
            width,
            height,
            8,
            4,
            true,
            false,
            NSDeviceRGBColorSpace,
            NSBitmapFormat::AlphaNonpremultiplied,
            0,
            0,
        )?
    };

    let ctx = NSGraphicsContext::graphicsContextWithBitmapImageRep(&rep)?;
    let prev = NSGraphicsContext::currentContext();
    NSGraphicsContext::setCurrentContext(Some(&ctx));

    // Bird, tinted to the text colour (silhouette PNG -> SourceAtop fill).
    let bird_rect = NSRect::new(
        NSPoint::new(bird_x, (h_px - px(BIRD_H)) / 2.0),
        NSSize::new(px(BIRD_W), px(BIRD_H)),
    );
    if let Some(bird) = NSImage::initWithData(
        NSImage::alloc(),
        &objc2_foundation::NSData::with_bytes(BIRD_PNG),
    ) {
        bird.drawInRect(bird_rect);
        ctx.setCompositingOperation(NSCompositingOperation::SourceAtop);
        text_color.setFill();
        NSBezierPath::fillRect(bird_rect);
        ctx.setCompositingOperation(NSCompositingOperation::SourceOver);
    }

    // Cost, vertically centred.
    cost.drawAtPoint(NSPoint::new(cost_x, (h_px - cost_size.height) / 2.0));

    if !rows.is_empty() {
        // Full-height faded pipe.
        pipe_color.setFill();
        NSBezierPath::fillRect(NSRect::new(
            NSPoint::new(pipe_x, px(PAD) + px(1.0)),
            NSSize::new(px(1.0), h_px - 2.0 * (px(PAD) + px(1.0))),
        ));

        // Stacked rows, vertically centred as a block. Origin is bottom-left,
        // so the first row (daily) gets the higher y to sit on top.
        let rows_h = ROW_H * rows.len() as f64;
        let block_bottom = (h_px - px(rows_h)) / 2.0;
        let n = rows.len();
        for (i, (text, size, band)) in rows.iter().enumerate() {
            let row_bottom = block_bottom + px(ROW_H) * (n - 1 - i) as f64;
            text.drawAtPoint(NSPoint::new(
                rows_x,
                row_bottom + (px(ROW_H) - size.height) / 2.0,
            ));

            let dot_x = rows_x + size.width + px(GAP_TEXT_DOT);
            let dot_y = row_bottom + (px(ROW_H) - px(DOT)) / 2.0;
            let oval = {
                NSBezierPath::bezierPathWithOvalInRect(NSRect::new(
                    NSPoint::new(dot_x, dot_y),
                    NSSize::new(px(DOT), px(DOT)),
                ))
            };
            dot_color(*band).setFill();
            oval.fill();
        }
    }

    NSGraphicsContext::setCurrentContext(prev.as_deref());

    let empty = NSDictionary::new();
    let png =
        unsafe { rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &empty) }?;
    Some(png.to_vec())
}
