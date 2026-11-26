# Third-party documents

This folder holds documents that this repository uses but does not redistribute: the
modulator vendor's firmware images, manuals and spec sheets, and the standards the server
implements. Everything here is gitignored except this file and, for each standards folder,
`SOURCES.txt` (where each file was downloaded) and `SHA256SUMS` (to check a download).

| Folder or file | What | Where it comes from |
|---|---|---|
| `standards/` | ITU-T, ATSC, CTA, SCTE and ETSI standards, and 47 CFR sections | `SOURCES.txt` in each subfolder |
| `eqam-420b.zip`, `bt-firmware-sharepoint/` | Blonder Tongue firmware images and readmes | Blonder Tongue's public firmware SharePoint (`notes/research.md`) |
| `blonder tongue eqam-420b.pdf` | EQAM-420B manual | A ManualsLib scrape (`notes/manual.md`) |
| `eqam-420a-manual-avprosupply/` | EQAM-420A manual | AVPro Supply (`notes/research.md`) |
| `sister-aqt8-manual/` | AQT8-QAM/IP manual (a sister product) | Full Compass |
| `bt-wayback/` | Vendor spec sheets and presentations | The Internet Archive's Wayback Machine (`notes/research.md`) |

The notes and code cite these files by their paths here, so put downloads at the same paths.
