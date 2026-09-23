# Image Viewer - Implementation Plan

## Features

### 1. Zoom Behavior
- **Zoom out (mouse wheel)**: No inertia - instant snap to new zoom level
- **Zoom in (mouse wheel)**: No inertia - instant snap to new zoom level
- **Current behavior**: Already snaps instantly (no inertia) - confirmed working

### 2. Load All Images (Queue)
- Add a "Load All" button in the UI
- When clicked, collect all image files from the selected folder
- Sort by filename (like Explorer)
- Load them sequentially, updating the gallery
- Each load updates the current image (not just the first)

### 3. Existing Features (already implemented)
- Drag-and-drop image selection
- Browse folder / Browse file picker (JPG/PNG)
- Gallery navigation (prev/next)
- Zoom in/out with mouse wheel (snapping)
- Browse button for single file
- Property tests, unit tests, benchmarks

## Technical Details

### Zoom Without Inertia
- Currently zoom already snaps (no inertia) - confirmed by test `test_zoom_pan_reset_and_clamp`
- No changes needed - the current implementation is correct

### Load All Images
- Add `load_all()` method to `Gallery` struct
- Method collects all files from `folder` path, sorts by filename
- Sets first file as current (or cycles through all)
- Should sort like Explorer (natural sort by filename)
- Triggered by a new button in the UI

## Implementation Order

1. **Fix zoom behavior** - confirm no inertia (already done)
2. **Add "Load All" button** - UI modification
3. **Implement `load_all()`** - core logic
4. **Update tests** - add test for load-all functionality
5. **Verify everything compiles and tests pass**

## Files to Modify
- `src/main.rs` - add button, load_all method
- `src/lib.rs` - add load_all method to Gallery
- `tests/standard.rs` - add test for load_all
- `tests/bench_variants.rs` - add benchmark for load_all
- `tests/integration.rs` - add test for load_all workflow
- `.gitignore` - ensure no accidental exclusions
