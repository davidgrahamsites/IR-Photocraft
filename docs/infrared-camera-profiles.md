# Infrared camera profiles

PhotoCraft's infrared workflow uses DNG camera profiles (`.dcp`) while developing the original
sensor data. A camera profile is separate from the document's ICC working space and from a
Camera Raw settings preset (`.xmp`). Import a profile made for the camera and conversion/filter
combination used to take the photograph.

## Follow the in-app tutorial

In the Camera Raw dialog, expand **Camera Profile / Infrared** and choose **IR profile tutorial**.
The guide stays beside the editing controls. Use **Next** and **Back** to move through its eight
steps, **Close** to leave at any point, or **Done** after the final step. Starting it again returns
to the first step; it does not change the photograph or require a step to be completed.

The tutorial covers opening a supported RAW, importing and applying a compatible DCP, sensor
white balance, before/after and highlight checks, creating a profile, exporting it and finishing
the image with Channel Mixer or Curves.

## Import and use an existing profile

1. Open a supported camera RAW file. Keep **Preferences → Raw Defaults → Open in Camera Raw**
   enabled to use the opening dialog.
2. In **Camera Profile / Infrared**, choose **Import DCP profiles…** and select one or more `.dcp`
   files. The imported
   library is saved with PhotoCraft's preferences; importing does not alter the original files.
   The library accepts up to 512 profiles, 16 MiB per file and 64 MiB in total.
3. Choose the desired profile and select **Apply profile / white balance**. Wait for the RAW
   preview to finish before comparing it or opening the image. The **Applied profile** label
   identifies the profile used by the completed preview.
4. For IR white balance, enable the custom sensor white balance and adjust the red, green and
   blue multipliers. Apply the preview again. These gains act on the RAW channels before the
   image is converted to the RGB editing space.
5. Open the developed image, then continue with the existing channel mixer, curves, color and
   monochrome tools.

The relative Temperature and Tint controls remain available for further adjustment. Direct
sensor gains are useful when an IR image needs a balance outside a conventional visible-light
temperature control. Neutralizing a chosen object is a creative choice; IR foliage, fabric and
gray targets can reflect differently from their visible-light appearance.

Camera profiles require decoded sensor data. Applying a DCP to an already rendered JPEG/TIFF,
or to an unsupported RAW file opened as an embedded JPEG preview, cannot recover that stage.

## Create and export an IR profile

Start with a compatible imported DCP, or a RAW file with usable embedded calibration. In the
**Create an infrared DCP** controls, enter a distinct name and the **Calibration shift (R / G / B)**
multipliers, then select **Create DCP profile**. Select it, apply it to the preview and inspect
both neutral areas and highlights. Use **Export selected DCP…** to save the selected profile as
a portable `.dcp` file.

Calibration multipliers change the profile's camera-to-color mapping. They are distinct from
the per-photo sensor white-balance gains. A new profile adjusts an existing calibration; it does
not measure a camera's spectral sensitivity or derive an IR colorimetric calibration from a
single photograph. Keep separate profiles for different camera conversions or filters where
their rendering differs.

## Relationship to the earlier Nikon IR workflow

The user-supplied `NikonIRinCamera` work includes camera-menu simulation and JPEG reference
images for Nikon's **PRE Preset manual → Use photo** workflow. Its D3100 IR preset README
describes first-pass references for 720 nm, 590 nm and IR Chrome B01/B02. The recorded recipes
use Direct sunlight with B6 + G6, or centered Incandescent white balance. Those references are
in-camera white-balance aids; they are not a portable DCP profile library.

The four Nikon coefficients in that README have a camera-specific ordering. Do not paste the
first three into PhotoCraft's R/G/B controls or treat them as a camera-to-XYZ calibration matrix.
Translating them requires establishing their channel meaning and validating the result against
the source RAW. The new DCP workflow preserves the distinction between a per-photo neutral
balance and the profile's calibration. Neither the earlier practical preset recipes nor a new
profile adjustment establishes measured IR spectral calibration.

Provenance reviewed for this comparison: `NikonIRinCamera/README_MAC_APP.md` and
`NikonIRinCamera/nikon_d3100_ir_filter_presets_v1/README.md`. No JPEGs or personal photographs from
that workspace are included in this repository.

## Automation

The same RAW development and profile-creation functions are exposed through engine commands:

```json
{"id":"raw.profile.inspect","params":{"path":"/profiles/Camera-720nm.dcp"}}
```

```json
{"id":"raw.open","params":{"path":"/photos/infrared.dng","profilePath":"/profiles/Camera-720nm.dcp","whiteBalanceMultipliers":[0.25,1.0,2.0]}}
```

```json
{"id":"raw.profile.create","params":{"profilePath":"/profiles/Base.dcp","path":"/profiles/My-IR.dcp","name":"My IR profile","whiteBalanceMultipliers":[0.5,1.0,1.5]}}
```

Profile creation can use `sourcePath` instead of `profilePath` to read a RAW file's calibration.
The example gains are illustrative, not a recommended balance for a particular camera.
These path-based commands are for trusted local engine/CLI use. Capability-scoped automation
rejects them because they access ambient filesystem paths.

## Compatibility and verification

The DCP reader accepts either byte order, one or two calibration illuminants, color and forward
matrices, hue/saturation maps, look tables, profile tone curves and baseline exposure offsets.
Table encodings are linear or sRGB value-axis encoding. The RAW API's file limit is 32 MiB;
the UI library uses the tighter import limits above.

Profiles requiring third illuminants, custom illuminant spectra, gain maps, RGB tables or newer
HDR rendering stages, or more than three camera channels, are outside this implementation's
scope. A color profile cannot be applied to a monochrome sensor. A profile declaring a camera
model must match the opened RAW's camera.

Malformed profiles and unsupported processing stages must produce a descriptive error. An
accepted profile must not silently lose a required rendering stage. Support for a particular
camera RAW container remains subject to the decoder's [support matrix](../crates/raw/README.md).

Synthetic fixtures exercise parsing and rendering independently of Adobe's installed profiles.
Real IR render parity with Adobe Camera Raw has not been established: no user IR RAW/profile
pair was supplied for comparison. Import/export interoperability must be distinguished from
pixel-identical output, especially for profile look tables, tone behavior and proprietary
adaptive processing.

## Investigation sources and boundaries

The investigation was scoped to importing, using and creating camera profiles for infrared
editing, rather than reconstructing every Adobe Camera Raw feature.

| Evidence | What it establishes | What it does not establish |
|---|---|---|
| Installed plug-in `Info.plist`, inspected with REA `inspect_plist` | Adobe Camera Raw 18.7 (2743), bundle `com.adobe.CameraRaw` | Executed rendering behavior or algorithm parity |
| PhotoCraft source at baseline `c65b5d25d3cfee71ecdb96b6549721e0deac8840` | Existing pure-Rust RAW decoder, developer and opening dialog | Adobe implementation details |
| [Adobe Camera Raw profile documentation](https://helpx.adobe.com/ph_fil/camera-raw/desktop/edit-and-enhance-images/tone-and-color/adjust-color-rendering-camera-camera.html) | Profile selection is separate from adjustment sliders; camera matching uses camera identity | Results with this user's custom IR profiles |
| [Adobe DNG Specification 1.7.1.0](https://helpx.adobe.com/content/dam/help/en/camera-raw/digital-negative/jcr_content/root/content/flex/items/position/position-par/download_section_733958301/download-1/DNG_Spec_1_7_1_0.pdf) | Public camera-profile container, matrices, tables and tone-curve format | A requirement to copy Adobe's profiles or rendering code |

The retained [REA identity Evidence](evidence/camera-raw-identity.json): `ev_0183a6f53d437b0d169a5dbc0cb68f29ed22e37a8cd7d6d82bbd4ac7258045ef`.
Executable SHA-256: `a2d6a7f2b7269256f29a0b17883348c7ff4511b6d397636c7cf3771a8a8f61d8`.
The inspected target was the installed `Camera Raw.plugin` executable. Adobe DNG Converter was
also installed, but no Photoshop host UI was available for a live Camera Raw comparison.

Implementation uses public format specifications and original Rust code. Adobe's code,
shaders, installed camera profiles and artwork are not bundled with PhotoCraft.

This product includes DNG technology under license by Adobe.
