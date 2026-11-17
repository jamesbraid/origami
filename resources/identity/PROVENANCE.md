# Identity artwork

The SGI tube cube, shared by the native apps.

Copied unmodified from `assets/` of the irix-theme repository
(`/home/jamesb/joy/tech/irix-theme`) at commit
`202a8c0f37c165fa486f7bf40ae476efcf8bb308`:

- `cube.glb`: the mesh (quantised, 1764 vertices, 3528 triangles, no normals;
  the renderer sums face normals per vertex)
- `granite-tile.png`: the IRIX desktop granite tile, two colours, drawn
  recoloured and pixel-doubled. irix-theme's `LICENSE` says "Silicon
  Graphics' material; no licence is stated for it."

The look is a port of `irix-theme/src/elements/irix-cube-gl.js` (chrome,
studio lighting and rotation) into `crates/origami-render/src/cube.wgsl`.

## Scene photographs

`scenes/` holds the equirect images the cube reflects and `scenes.json`,
which lists them in the order Alt-click cycles, carries each one's credit
line, and names the scene About shows in each appearance. A new scene is a
1024 by 512 WebP and a new entry; the build embeds them.

Each is a freely licensed photograph from Wikimedia Commons, cropped, laid
round the horizon and mirror-tiled over 70 degrees of latitude, with sky and
ground faded from the photograph's own top and bottom rows, in the design
pass. The Commons originals are not kept here. The credit line must be shown in the
app's acknowledgements.

| File | Photograph | Author, year, licence |
|---|---|---|
| `howe-sound.webp` | Howe Sound from Horseshoe Bay | Roy Luck, 2011, CC BY 2.0 |
| `garibaldi-lake.webp` | View towards Garibaldi Lake from Panorama Ridge | Benoit Brummer (Trougnouf), 2018, CC BY 4.0 |
| `vancouver-dusk.webp` | Vancouver from Grouse Mountain | Adam Lindsay, 2004, CC BY 2.0 |

## Source credit and trademark

The model comes from `sgi-logo.3ds`, offered at
<https://www.martin-steen.de/sgi/cube.html>, which states no licence. The
maintainer decided to ship it with credit. The apps' About page says:

> SGI cube model from martin-steen.de.
> All trademarks are the property of their respective owners.

This wording is pending the maintainer's review.

From irix-theme's `LICENSE`:

> The cube mark (assets/mark.svg, assets/mark-variants.css, assets/cube.glb,
> the favicons, src/elements/irix-cube.js, src/elements/irix-cube-gl.js)
>   The SGI tube cube is a Silicon Graphics trademark. The shapes are rendered
>   by hinv-archive's art/mark tools from sgi-logo.3ds, a model offered at
>   https://www.martin-steen.de/sgi/cube.html. That page names no author and
>   states no licence, only a trademark notice. The renderer and element code
>   were written for hinv-archive; hinv-archive's art/mark/ is their source.
