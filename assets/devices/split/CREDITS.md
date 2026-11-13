# Split models: sources and credits

Split into one named part per control on 11 November 2026 (`xbox_split.py`, `akai_split.py`, Blender 5.2, run with scripts disabled for the source files). Node names follow `docs/v7/spec/3d-device-models.md`.

| File | Source | Author | Licence |
|---|---|---|---|
| `xbox-one-white-split.glb` | `xbox_inalambric_controller_white.glb`, https://sketchfab.com/3d-models/xbox-inalambric-controller-white-f18a70fc10414ef5a39b55de68f12823 | DogexorRexUwU (https://sketchfab.com/oscar.lopez.riviello) | CC-BY-4.0: credit required in any release |
| `xbox-one-black-split.glb` | `xbox_inalambric_controller_black.glb`, same upload | DogexorRexUwU | CC-BY-4.0: credit required |
| `dualsense-split.glb` | `playstation_5_dualsense.glb`, https://sketchfab.com/3d-models/playstation-5-dualsense-878c1f882808477ab81c2fe86d5a3936 | AHarmlessPotato (https://sketchfab.com/AHarmlessPotato) | CC-BY-4.0: credit required |
| `mpk-mini-blend-split.glb` | `AKAI MPK Mini.blend` (downloaded) | unknown: the file names none | **unknown: no licence in the file. Don't ship it publicly until the licence is known.** |

Notes:
- The Xbox One controller has no Share button, so `xbox_split.py` adds `button_share`; `port_usb_c` and `port_headset` stay part of `body`.
- The source has no grip texture: `xbox_split.py` adds the dotted normal map (ticket 38's, now defined in `xbox_split.py`) to the triggers, bumpers and the two grip panels on the back (the source's own mesh islands; the battery door and the centre of the back stay smooth), as one shared `grip_dotted` material on its own `grip` UV map (glTF `TEXCOORD_3`). The source already has its parting line: the white (or black) top shell meets the dark back case, so no seam is added. View, Menu and Share keep the split script's own prints (visible; no dark rings as on ticket 38's model).
- The Xbox logo and button letters are in the source textures. The repo is public: logos must go before a public release (spec: "no brand logos or wordmarks").
- The `.blend`'s label and screen textures (`mpk_decal_thomas2.png`, `mpk_decal_btn.png`, `mpk_oled.png`) weren't in the download, so labels are blank.
- The DualSense's light bar (two strips beside the touchpad), player LEDs (`player_led_1`–`_5`) and Mute light are added by `ps5_split.py`: the download doesn't model them separately. Its d-pad is four separate buttons, as on the real controller. Textures are cut to 2K JPEG (48.8 MB → 4.5 MB). Sony and PlayStation logos are in the textures.
- The Xbox d-pad is one part (`dpad`) that rocks toward each of 4 directions, as the real one does.
