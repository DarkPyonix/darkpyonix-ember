// usage: osascript -l JavaScript window-id.js <pid>
// Prints the CoreGraphics window id of the largest on-screen normal window owned by <pid>, or
// nothing when it has none. side-by-side.sh captures by this id, so the front-most window of
// any other application is never touched (FR-L37, #222).
ObjC.import('CoreGraphics');
ObjC.import('Foundation');

function run(argv) {
  var pid = parseInt(argv[0], 10);
  if (isNaN(pid)) { return ''; }
  var list = ObjC.deepUnwrap(ObjC.castRefToObject(
    $.CGWindowListCopyWindowInfo($.kCGWindowListOptionAll, 0)));
  var best = null;
  var area = 0;
  for (var i = 0; i < list.length; i++) {
    var w = list[i];
    if (w.kCGWindowOwnerPID !== pid || w.kCGWindowIsOnscreen !== true) { continue; }
    if (w.kCGWindowLayer !== 0) { continue; }
    var a = w.kCGWindowBounds.Width * w.kCGWindowBounds.Height;
    if (a > area) { area = a; best = w; }
  }
  return best ? String(best.kCGWindowNumber) : '';
}
