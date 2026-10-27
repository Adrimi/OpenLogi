# Desktop-only artwork

Files here are embedded by `src/app_assets.rs` and served only to the settings
app; the overlay never links them (shared artwork lives in `openlogi-ui`). They
live outside `assets/`, which `cargo xtask macos bundle` empties on every build
to stage downloaded device renders.

## `wireless-mouse.svg`

Original artwork drawn by hand for this project (a generic top-down wireless
mouse outline). It is not traced from, derived from, or based on any photo,
render, logo, or trademark of any manufacturer, and it embeds no raster data.
It ships under the same MIT/Apache-2.0 terms as the code. It is a single-colour
stroke (`currentColor`), tinted by the app palette, used as the gallery
placeholder for mice and trackballs with no downloaded device render.
