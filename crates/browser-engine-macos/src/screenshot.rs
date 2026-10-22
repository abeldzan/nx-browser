//! Page and element screenshots. WebKit's snapshot only paints what is on screen and leaves the rest blank, so the
//! target is captured a screenful at a time, scrolling between slices, and the slices are stitched into one PNG.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc::Sender;

use block2::RcBlock;
use browser_engine::{EngineEvent, ScreenshotArea};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{msg_send, AllocAnyThread, MainThreadMarker, Message};
use objc2_app_kit::{NSBitmapImageFileType, NSBitmapImageRep, NSDeviceRGBColorSpace, NSGraphicsContext, NSImage};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::{NSDictionary, NSError, NSNull, NSPoint, NSRect, NSSize, NSString};
use objc2_web_kit::{WKContentWorld, WKSnapshotConfiguration, WKWebView};

/// The whole document: "x,y,width,height" in document CSS px.
const PAGE_JS: &str = r#"const e = document.documentElement;
return [0, 0, e.clientWidth, Math.max(e.scrollHeight, document.body ? document.body.scrollHeight : 0)].join(',');"#;

/// Lets the user pick an element: "x,y,width,height" in document CSS px, or null when cancelled.
const PICK_JS: &str = include_str!("screenshot.js");

/// Scrolls to `{y}` (document CSS px; NaN stays put) and, when `{hide}`, hides fixed and sticky elements so headers
/// show once instead of on every slice. Returns "scrollX,scrollY,viewportWidth,viewportHeight".
const STEP_JS: &str = r#"if ({hide} && !window.nxShotHidden) {
  window.nxShotHidden = [...document.querySelectorAll('body *')]
    .filter((el) => /fixed|sticky/.test(getComputedStyle(el).position))
    .map((el) => [el, el.style.getPropertyValue('visibility'), el.style.getPropertyPriority('visibility')]);
  for (const [el] of window.nxShotHidden) el.style.setProperty('visibility', 'hidden', 'important');
}
const y = {y};
if (!Number.isNaN(y)) {
  scrollTo({ left: scrollX, top: y, behavior: 'instant' });
  // Gives lazy images a moment to load in.
  await new Promise((done) => setTimeout(done, 250));
}
// Two frames, so what's painted matches the scroll.
await new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(done)));
const e = document.documentElement;
return [scrollX, scrollY, Math.min(e.clientWidth, innerWidth), Math.min(e.clientHeight, innerHeight)].join(',');"#;

/// Undoes `STEP_JS`: shows the hidden elements and scrolls back to `{x},{y}` (NaN: stays put).
const RESTORE_JS: &str = r#"for (const [el, value, priority] of window.nxShotHidden || []) el.style.setProperty('visibility', value, priority);
window.nxShotHidden = null;
if (!Number.isNaN({y})) scrollTo({ left: {x}, top: {y}, behavior: 'instant' });"#;

/// Most screenfuls in one image, so endless feeds still finish.
const MAX_SLICES: usize = 40;

struct Shot {
    view: Retained<WKWebView>,
    tx: Sender<EngineEvent>,
    path: PathBuf,
    /// The area to capture, in document CSS px.
    target: Cell<(f64, f64, f64, f64)>,
    /// Where the page was scrolled before the capture, to go back to.
    scroll: Cell<Option<(f64, f64)>>,
    /// Document y down to which the target is captured.
    covered: Cell<f64>,
    /// Captured slices and their top within the image, in points.
    slices: RefCell<Vec<(Retained<NSImage>, f64)>>,
}

/// Writes `area` of the page to `path` as PNG; `EngineEvent::Screenshot` reports it.
/// Nothing is reported when the user cancels an element pick.
pub(crate) fn capture(view: Retained<WKWebView>, tx: Sender<EngineEvent>, area: ScreenshotArea, path: PathBuf) {
    let shot = Rc::new(Shot {
        view,
        tx,
        path,
        target: Cell::new((0.0, 0.0, 0.0, 0.0)),
        scroll: Cell::new(None),
        covered: Cell::new(0.0),
        slices: RefCell::new(Vec::new()),
    });
    let view = shot.view.clone();
    let script = match area {
        ScreenshotArea::Page => PAGE_JS,
        ScreenshotArea::Element => PICK_JS,
    };
    run_js(&view, script, move |result| match result {
        Err(error) => shot.fail(error),
        Ok(None) => {}
        Ok(Some(text)) => match parse_numbers(&text) {
            Some([x, y, width, height]) if width >= 1.0 && height >= 1.0 => {
                shot.target.set((x, y, width, height));
                shot.covered.set(y);
                step(shot, f64::NAN);
            }
            _ => shot.fail("nothing to capture".into()),
        },
    });
}

/// Scrolls to `y` (NaN: stays put), then captures the part of the target on screen and moves on to the next.
fn step(shot: Rc<Shot>, y: f64) {
    let hide = !shot.slices.borrow().is_empty();
    let script = STEP_JS.replace("{hide}", &hide.to_string()).replace("{y}", &js_number(y));
    let view = shot.view.clone();
    run_js(&view, &script, move |result| {
        let Some([scroll_x, scroll_y, view_width, view_height]) = result.ok().flatten().as_deref().and_then(parse_numbers) else {
            return shot.fail("the page stopped responding".into());
        };
        if shot.scroll.get().is_none() {
            shot.scroll.set(Some((scroll_x, scroll_y)));
            // shortcut: only the on-screen width is kept; scroll sideways too if pages wider than the window matter.
            let (x, y, width, height) = shot.target.get();
            let (left, right) = (x.max(scroll_x), (x + width).min(scroll_x + view_width));
            if right - left < 1.0 {
                return shot.fail("the element is off to the side".into());
            }
            shot.target.set((left, y, right - left, height));
            // Already all on screen: one slice, no scrolling. Otherwise start from its top.
            if y < scroll_y || y + height > scroll_y + view_height {
                return step(shot, y);
            }
        }
        let (left, y, width, height) = shot.target.get();
        let top = shot.covered.get().max(scroll_y);
        let bottom = (y + height).min(scroll_y + view_height);
        // The page can't scroll any further (it shrank while capturing).
        if bottom - top < 1.0 {
            return finish(&shot);
        }
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        // shortcut: pinch magnification isn't applied, only page zoom; add `magnification()` if pinch-zoomed shots come out offset.
        let zoom = unsafe { shot.view.pageZoom() };
        let config = unsafe { WKSnapshotConfiguration::new(mtm) };
        let rect = CGRect::new(CGPoint::new((left - scroll_x) * zoom, (top - scroll_y) * zoom), CGSize::new(width * zoom, (bottom - top) * zoom));
        unsafe { config.setRect(rect) };
        let next = shot.clone();
        let done = RcBlock::new(move |image: *mut NSImage, error: *mut NSError| {
            let Some(image) = (unsafe { image.as_ref() }) else {
                return next.fail(unsafe { error.as_ref() }.map_or_else(|| "snapshot failed".into(), |error| error.localizedDescription().to_string()));
            };
            let count = {
                let mut slices = next.slices.borrow_mut();
                slices.push((image.retain(), (top - y) * zoom));
                slices.len()
            };
            next.covered.set(bottom);
            if bottom < y + height && count < MAX_SLICES {
                step(next.clone(), bottom);
            } else {
                finish(&next);
            }
        });
        unsafe { shot.view.takeSnapshotWithConfiguration_completionHandler(Some(&config), &done) };
    });
}

fn finish(shot: &Shot) {
    shot.restore();
    let (_, y, width, _) = shot.target.get();
    let zoom = unsafe { shot.view.pageZoom() };
    let size = NSSize::new(width * zoom, (shot.covered.get() - y) * zoom);
    let written = stitch(&shot.slices.borrow(), size)
        .ok_or_else(|| "couldn't encode the image".to_string())
        .and_then(|data| std::fs::write(&shot.path, data).map_err(|error| error.to_string()));
    shot.report(written.err());
}

impl Shot {
    fn restore(&self) {
        let (x, y) = self.scroll.get().unwrap_or((f64::NAN, f64::NAN));
        run_js(&self.view, &RESTORE_JS.replace("{x}", &js_number(x)).replace("{y}", &js_number(y)), |_| {});
    }

    fn report(&self, error: Option<String>) {
        let _ = self.tx.send(EngineEvent::Screenshot { path: self.path.display().to_string(), error });
    }

    fn fail(&self, error: String) {
        self.restore();
        self.report(Some(error));
    }
}

/// Draws `slices` (image, top in points) one under another into a PNG `size` points big.
fn stitch(slices: &[(Retained<NSImage>, f64)], size: NSSize) -> Option<Vec<u8>> {
    let (first, _) = slices.first()?;
    // Slices come at the screen's backing scale; the image keeps it.
    let scale = first.representations().firstObject()?.pixelsWide() as f64 / first.size().width;
    let rep = unsafe {
        NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bytesPerRow_bitsPerPixel(
            NSBitmapImageRep::alloc(),
            std::ptr::null_mut(),
            (size.width * scale).round() as isize,
            (size.height * scale).round() as isize,
            8,
            4,
            true,
            false,
            NSDeviceRGBColorSpace,
            0,
            0,
        )
    }?;
    rep.setSize(size);
    let context = NSGraphicsContext::graphicsContextWithBitmapImageRep(&rep)?;
    NSGraphicsContext::saveGraphicsState_class();
    NSGraphicsContext::setCurrentContext(Some(&context));
    for (image, top) in slices {
        let slice = image.size();
        // AppKit's origin is bottom-left.
        image.drawInRect(NSRect::new(NSPoint::new(0.0, size.height - top - slice.height), slice));
    }
    NSGraphicsContext::restoreGraphicsState_class();
    let data = unsafe { rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new()) }?;
    Some(data.to_vec())
}

/// Runs `script` as an async function body in the app's own JS world; `done` gets its result as text, `None` for null.
fn run_js(view: &WKWebView, script: &str, done: impl FnOnce(Result<Option<String>, String>) + 'static) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let done = Cell::new(Some(done));
    let block = RcBlock::new(move |value: *mut AnyObject, error: *mut NSError| {
        let Some(done) = done.take() else {
            return;
        };
        // JS `null` arrives as NSNull, `undefined` as nil; both mean no result.
        let value = unsafe { value.as_ref() }.filter(|value| value.downcast_ref::<NSNull>().is_none());
        done(match (unsafe { error.as_ref() }, value) {
            (Some(error), _) => Err(error.localizedDescription().to_string()),
            (None, None) => Ok(None),
            (None, Some(value)) => {
                let text: Retained<NSString> = unsafe { msg_send![value, description] };
                Ok(Some(text.to_string()))
            }
        });
    });
    unsafe {
        view.callAsyncJavaScript_arguments_inFrame_inContentWorld_completionHandler(
            &NSString::from_str(script),
            None,
            None,
            &WKContentWorld::defaultClientWorld(mtm),
            Some(&block),
        );
    }
}

fn js_number(value: f64) -> String {
    if value.is_finite() { value.to_string() } else { "NaN".into() }
}

/// Four comma-separated numbers.
fn parse_numbers(text: &str) -> Option<[f64; 4]> {
    let parts = text.split(',').map(|part| part.trim().parse::<f64>().ok()).collect::<Option<Vec<_>>>()?;
    parts.try_into().ok()
}

#[cfg(test)]
mod tests {
    use super::{js_number, parse_numbers};

    #[test]
    fn reads_four_numbers_only() {
        assert_eq!(parse_numbers("-10, 0,200.5,40"), Some([-10.0, 0.0, 200.5, 40.0]));
        assert_eq!(parse_numbers("0,0,40"), None);
        assert_eq!(parse_numbers("null"), None);
        assert_eq!(js_number(f64::NAN), "NaN");
        assert_eq!(js_number(12.5), "12.5");
    }
}
