# takumi-raster

<!-- cargo-rdme start -->

Raster painting backend for takumi, built on tiny-skia.

[`render`] paints a node tree into a [`Bitmap`](https://docs.rs/takumi-raster/latest/takumi_raster/write/struct.Bitmap.html), and [`write_image`](https://docs.rs/takumi-raster/latest/takumi_raster/write/fn.write_image.html) encodes it as PNG,
JPEG, WebP, or ICO. [`write_animation`](https://docs.rs/takumi-raster/latest/takumi_raster/write/fn.write_animation.html) encodes animated WebP, APNG, or GIF.

Use it through the `takumi` crate, which re-exports these functions at its root. The whole
crate is `takumi::unstable::raster`. This crate does not re-export core types; reach them
through `takumi_core` or `takumi::unstable::base`.

<!-- cargo-rdme end -->
