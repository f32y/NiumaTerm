# Colors

`Rgba` parses RGB and RGBA hex strings into normalized sRGB components.
Alpha defaults to 1.0 when the input has six digits.

Rendering colors use `ColorArray`, an `[f32; 4]` containing normalized red,
green, blue, and alpha values. Background uses the same representation.

```rust
let color: ColorArray = Rgba::from_hex("#151515".into()).unwrap().into();

assert_eq!(color, [21.0 / 255.0, 21.0 / 255.0, 21.0 / 255.0, 1.0]);
```
