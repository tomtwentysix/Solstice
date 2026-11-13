# Starter content candidates

Candidates for the captures and cabinet impulse responses that may ship with Solstice. Nothing
here has been downloaded into the repository, and nothing ships until Tom approves it. Research
done 13 November 2026.

Solstice is GPL-3.0-or-later, so content under GPL-2.0-or-later, GPL-3.0, CC0, CC BY 4.0 or
CC BY-SA 4.0 can be bundled. CC BY and CC BY-SA need the attribution below in the README and RPM
credits; CC BY-SA 4.0 is one-way compatible with GPLv3. Gear names appear only where they are part
of a file's own name, so that each candidate can be found again.

## Captures

### Source A: NAM_models community collection (GPL-3.0)

- Repository: https://github.com/pelennor2170/NAM_models
- Licence evidence: the repository's `COPYING` is the GNU GPL v3, GitHub reports `GPL-3.0`, and
  the README states: "These files are licenced under the GNU GPL v3, please see the file COPYING
  for more details." The licence was added in the commit "Licence under GPL v3" (9 February 2023).
- Caveat: every file was committed by the one maintainer account, credited in commit messages as
  "Models courtesy of" each author. There is no record that each author agreed to the GPL; the
  maintainer licensed the collection on their behalf. A later commit removed captures of software
  "due to IP concerns". Tom should decide whether the maintainer's statement is enough, or whether
  to ask the individual authors to confirm.
- Exclude anything captured from a commercial modeller or plug-in (the files with "Axe FX" or
  "Line 6" in the name); TONE3000's own guidelines bar those for licence reasons too.
- All are WaveNet captures with no `sample_rate` field, so they are 48 kHz (the format default).
  Most are amp or pedal only and need a cabinet impulse response; the `-Cab` variants include one.

| Use | File | Author (from file name) |
|---|---|---|
| Clean | `Tim R Fender TwinVerb Norm Bright.nam` | Tim R. |
| Clean | `Tim R JCM2000 Clean.nam` | Tim R. |
| Clean | `Phillipe P Bug333-Clean-ESR0,002.nam` | Phillipe P. |
| Crunch | `Tim R JCM2000 Crunch.nam` | Tim R. |
| Crunch | `Phillipe P VOXAC15-TopBoost.nam` | Phillipe P. |
| Crunch | `Phillipe P Bug333-Crunch-NoDrive-ESR0,002.nam` | Phillipe P. |
| High gain | `Helga B 5150 BlockLetter - NoBoost.nam` | Helga B. |
| High gain | `Helga B 6505+ Red ch - NoBoost.nam` | Helga B. |
| High gain | `Phillipe P JVM-OD2-RD-NoDrive-ESR0,003.nam` | Phillipe P. |
| Bass | `Jason Z Tech21 dUg DP3X bass preamp pedal all dimed no shift.nam` | Jason Z. |

The bass entry is the only bass capture in the collection, and it is a preamp pedal set fully up,
so it is a driven tone rather than a clean bass amp.

### Source B: NeuralAmpModelerCore example models (MIT)

- Already vendored at `namshim/NeuralAmpModelerCore/example_models`, covered by the repository's
  MIT `LICENSE` (Copyright (c) 2023 Steven Atkinson), which is already in the credits.
- `wavenet.nam` and `lstm.nam` carry metadata for a clean bass preamp captured by the author, but
  they are 2–4 KB test models made for the unit tests, not finished captures. The others are
  generated or unlabelled test files.
- Usable only as a last-resort bass placeholder. Not recommended.

### Rejected

- TONE3000: most captures carry the "T3K" licence, whose terms do not grant redistribution. Some
  are labelled CC BY, but the spec rules the site out as a source for now, and each would need its
  own check.
- GuitarML tone library: the plug-in code is GPL-3.0 or Apache-2.0, but no licence for the model
  files was found.

## Cabinet impulse responses

### Source C: Freesound, jesterdyne, "Brutal BG412S (V30+DV77) Cabinet IRs" (CC0)

- Pack: https://freesound.org/people/jesterdyne/packs/34071/ (6 files)
- Example: https://freesound.org/people/jesterdyne/sounds/618897/
- Licence evidence: each sound page shows "Creative Commons 0" with the summary "You can copy,
  modify, distribute and perform the sound". CC0 text:
  https://creativecommons.org/publicdomain/zero/1.0/
- A real modified 4x12 cabinet with mixed speakers, mostly close-miked with a dynamic microphone;
  one file mixes both speakers. Mono, 48 kHz, 24-bit, about 1.3 s.
- Use: crunch and high gain. Each file's licence should be confirmed before shipping.

### Source D: Freesound, tosha73, "IR Brown Cab_Audix i5 Center" (CC0)

- https://freesound.org/people/tosha73/sounds/507167/
- Licence evidence: the sound page shows "Creative Commons 0".
- A single guitar speaker close-miked at the centre. Mono, 96 kHz, 24-bit, 0.5 s.
- Use: clean and crunch.

### Source E: Freesound, mansardian, "Bristol" (CC BY 4.0)

- https://freesound.org/people/mansardian/sounds/648392/
- Licence evidence: the sound page shows "Attribution 4.0", linking to
  https://creativecommons.org/licenses/by/4.0/
- A blend of two guitar cabinets with four microphones, mixed for rock rhythm. Mono, 48 kHz,
  24-bit, 0.157 s.
- Attribution needed: "Bristol" by mansardian, Freesound, CC BY 4.0.
- Use: crunch and high gain.

### Source F: Guitarix built-in cabinets (GPL-2.0-or-later)

- Source: https://github.com/brummer10/guitarix/blob/master/trunk/src/LV2/DSP/cab_data.cc
- Licence evidence: the file header reads "Copyright (C) 2012 Hermann Meyer, Andreas Degert, Pete
  Shorthose, Steve Poskitt", under the GNU GPL "either version 2 of the License, or (at your
  option) any later version".
- 18 impulse responses held as float arrays in the source, 48 kHz (one at 44.1 kHz), 68–1000
  samples (1.4–21 ms). They are short and darker than modern captures, but include the only
  bass-oriented cabinets found under a usable licence: `1x15`, `4x10` and the `Bassman` entry.
- They would need exporting to WAV by a small script that reads the arrays. Where the original
  recordings came from is not stated; in a 2012 thread the Guitarix maintainer said the project
  only takes impulse responses with explicit permission to redistribute.
- Use: the bass cabinet (`1x15` or `4x10`), and a fallback for guitar.

### Rejected or unresolved

- Zach Weeks, "TL806 and Basement 1960B": the Gumroad listing
  (https://zachweeks.gumroad.com/l/tl806cabir) is indexed as stating CC BY-SA 4.0 (the page
  itself would not load its text for checking), but the same pack on Bandcamp
  (https://zachweeks.bandcamp.com/album/tl806-and-basement-1960b-guitar-cab-impulse-responses)
  says "all rights reserved", and the Gumroad page says it is no longer maintained. Usable only if
  the author confirms CC BY-SA 4.0.
- jesterdyne, "Speaker Impulse Responses" (Freesound pack 6385): CC0, but captured from a
  commercial modeller's cabinet simulation, not real cabinets. Excluded for the same reason as
  captures of software.
- Soundwoofer: described by its makers as public domain, but no licence text was found and the
  site refused connections.
- Origin Effects, God's Cab, Metal Cabs, forward audio, Shift Line and similar free packs: free to
  download, but no redistribution grant was found.
- No bass cabinet impulse response under CC0 or CC BY was found on Freesound.

## Suggested set

| Starter | Capture | Impulse response |
|---|---|---|
| Clean | `Tim R Fender TwinVerb Norm Bright.nam` (A) | tosha73 Brown Cab (D) |
| Crunch | `Phillipe P VOXAC15-TopBoost.nam` (A) | mansardian Bristol (E) |
| High gain | `Helga B 5150 BlockLetter - NoBoost.nam` (A) | jesterdyne BG412S Godmode (C) |
| Bass | `Jason Z Tech21 dUg DP3X …` (A) | Guitarix `1x15` (F) |

Questions for Tom:

1. Is the NAM_models maintainer's GPL statement enough, or should the capture authors be asked?
2. Is a driven bass preamp acceptable as the one bass capture, or should Bass open empty?
3. Are the Guitarix arrays acceptable as exported WAV files, given their unrecorded origin?
