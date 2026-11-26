| Input | Mbit/s | fps at 1 / 2 / 4 / 8 / 16 streams | NVDEC % at those | real-time streams (fps total / source fps) | paced: streams -> lateness first s -> last s, ms | host CPU per stream, cores | device MB per stream | software: cores per real-time stream |
|---|---|---|---|---|---|---|---|---|
| `av1_1080p30.mkv` 1920x1080 nv12 | 4.3 | 1661 / 1816 / 1807 / 1802 / 1806 | 89 / 100 / 100 / 100 / 100 | 60.6 | 40: 53 -> 16; 40: 51 -> 16; 40: 47 -> 16 | 0.007 | 101 | 0.18 |
| `h264_1080i.mkv` 1920x1080 nv12 interlaced | 10.1 | 834 / 831 / 831 / 825 / 840 | 100 / 100 / 100 / 100 / 100 | 28.0 | 22: 3 -> 1; 26: 142 -> 1; 30: 358 -> 961 | 0.006 | 65 | 0.23 |
| `h264_1080p24.mkv` 1920x1080 nv12 | 8.2 | 907 / 903 / 902 / 902 / 901 | 100 / 100 / 100 / 100 / 100 | 37.8 | 30: 16 -> 1; 35: 185 -> 1; 40: 387 -> 812 | 0.005 | 65 | 0.18 |
| `h264_1080p25.mkv` 1920x1080 nv12 | 8.1 | 912 / 909 / 907 / 907 / 906 | 100 / 100 / 100 / 100 / 100 | 36.5 | 29: 14 -> 1; 34: 167 -> 1; 40: 441 -> 1126 | 0.005 | 65 | 0.17 |
| `h264_1080p30.mkv` 1920x1080 nv12 | 8.1 | 925 / 922 / 921 / 921 / 919 | 100 / 100 / 100 / 100 / 100 | 30.9 | 24: 2 -> -2; 29: 155 -> -1; 33: 369 -> 876 | 0.005 | 65 | 0.19 |
| `h264_1080p60.mkv` 1920x1080 nv12 | 12.1 | 937 / 934 / 933 / 931 / 932 | 100 / 100 / 100 / 100 / 100 | 15.6 | 12: 1 -> 0; 14: 11 -> 1; 17: 274 -> 948 | 0.009 | 65 | 0.32 |
| `h264_1366x768p30.mkv` 1366x768 nv12 | 3.1 | 1674 / 1671 / 1669 / 1667 / 1665 | 100 / 100 / 100 / 100 / 100 | 55.9 | 40: 8 -> -1; 40: 11 -> -1; 40: 7 -> -1 | 0.004 | 35 | 0.08 |
| `h264_480i.mkv` 704x480 nv12 interlaced | 2.6 | 3302 / 3285 / 3284 / 3280 / 3278 | 100 / 100 / 100 / 100 / 100 | 110.2 | 40: 1 -> 1; 40: 1 -> 1; 40: 1 -> 1 | 0.004 | 12 | 0.05 |
| `h264_720p50.mkv` 1280x720 nv12 | 6.0 | 1816 / 1811 / 1808 / 1809 / 1804 | 100 / 100 / 100 / 100 / 100 | 36.3 | 29: 6 -> 1; 34: 153 -> 1; 39: 386 -> 967 | 0.007 | 35 | 0.13 |
| `h264_720p60.mkv` 1280x720 nv12 | 6.1 | 1839 / 1832 / 1829 / 1830 / 1826 | 100 / 100 / 100 / 100 / 100 | 30.7 | 24: 1 -> 1; 29: 120 -> 1; 33: 334 -> 928 | 0.008 | 35 | 0.15 |
| `h264_854x480p30.mkv` 854x480 nv12 | 1.5 | 3179 / 3167 / 3167 / 3161 / 3161 | 100 / 100 / 100 / 100 / 100 | 106.1 | 40: 1 -> -2; 40: 1 -> -2; 40: 1 -> -2 | 0.004 | 14 | 0.03 |
| `hevc_1080p30.mkv`* 1920x1080 nv12 | 5.1 | 1983 / 2077 / 2061 / 2059 / 2058 | 92 / 100 / 100 / 100 / 100 | 69.3 | 40: 6 -> 2; 40: 7 -> 2; 40: 6 -> 2 | 0.007 | 75 | 0.24 |
| `hevc_1080p30_10.mkv`* 1920x1080 p010le | 6.4 | 1838 / 1879 / 1876 / 1875 / 1879 | 94 / 100 / 100 / 100 / 100 | 62.7 | 40: 11 -> 1; 40: 11 -> 1; 40: 10 -> 1 | 0.006 | 121 | 0.30 |
| `hevc_2160p24_10.mkv` 3840x2160 p010le | 32.6 | 461 / 469 / 471 / 470 | 96 / 100 / 100 / 100 | 19.6 | 12: 4 -> 2; 12: 4 -> 2; 12: 4 -> 2 | 0.010 | 431 | 1.37 |
| `hevc_2160p30.mkv`* 3840x2160 nv12 | 27.0 | 529 / 537 / 537 / 538 | 97 / 100 / 100 / 100 | 18.0 | 12: 4 -> 1; 12: 4 -> 1; 12: 4 -> 1 | 0.013 | 235 | 1.05 |
| `mpeg2_1080i.ts`* 1920x1080 nv12 interlaced | 15.0 | 810 / 870 / 865 / 864 / 863 | 92 / 100 / 100 / 100 / 100 | 29.0 | 23: 9 -> 10; 27: 100 -> 14; 31: 305 -> 864 | 0.008 | 42 | 0.07 |
| `mpeg2_480i.ts`* 704x480 nv12 interlaced | 4.0 | 2801 / 3235 / 3231 / 3222 / 3219 | 85 / 100 / 100 / 100 / 100 | 107.9 | 40: 5 -> 5; 40: 5 -> 5; 40: 4 -> 5 | 0.005 | 10 | 0.01 |
| `vp9_1080p30.mkv`* 1920x1080 nv12 | 6.1 | 1430 / 1678 / 1675 / 1670 / 1673 | 84 / 100 / 100 / 100 / 100 | 56.0 | 40: 180 -> 14; 40: 139 -> 14; 40: 125 -> 14 | 0.010 | 114 | 0.16 |

| Input | 4 streams, hwaccel, copy into a pool frame (default) | hwaccel, decoder surfaces (`unsafe_output`) | decoders in a context of their own | `*_cuvid` decoder | cuvid adaptive deinterlace, one frame per frame | adaptive, one per field | bob, one per field |
|---|---|---|---|---|---|---|---|
| `av1_1080p30.mkv` | 1807 fps, SM 7 %, 74 us CPU/frame, 100 MB | 1814 fps, SM 5 %, 70 us CPU/frame, 172 MB | 1805 fps, SM 7 %, 90 us CPU/frame, 102 MB | 1811 fps, SM 7 %, 209 us CPU/frame, 68 MB | - | - | - |
| `h264_1080i.mkv` | 831 fps, SM 3 %, 109 us CPU/frame, 65 MB | 831 fps, SM 2 %, 120 us CPU/frame, 93 MB | 831 fps, SM 3 %, 142 us CPU/frame, 65 MB | 828 fps, SM 3 %, 177 us CPU/frame, 50 MB | 819 fps, SM 34 %, 197 us CPU/frame, 85 MB | 1603 fps, SM 63 %, 130 us CPU/frame, 87 MB | 1655 fps, SM 4 %, 100 us CPU/frame, 53 MB |
| `h264_1080p24.mkv` | 902 fps, SM 3 %, 110 us CPU/frame, 65 MB | 904 fps, SM 2 %, 96 us CPU/frame, 93 MB | 903 fps, SM 3 %, 120 us CPU/frame, 65 MB | 901 fps, SM 3 %, 216 us CPU/frame, 53 MB | - | - | - |
| `h264_1080p25.mkv` | 907 fps, SM 3 %, 116 us CPU/frame, 65 MB | 910 fps, SM 2 %, 102 us CPU/frame, 93 MB | 909 fps, SM 3 %, 121 us CPU/frame, 65 MB | 910 fps, SM 3 %, 219 us CPU/frame, 53 MB | - | - | - |
| `h264_1080p30.mkv` | 921 fps, SM 3 %, 123 us CPU/frame, 65 MB | 923 fps, SM 2 %, 99 us CPU/frame, 93 MB | 923 fps, SM 3 %, 118 us CPU/frame, 65 MB | 922 fps, SM 3 %, 215 us CPU/frame, 53 MB | - | - | - |
| `h264_1080p60.mkv` | 933 fps, SM 3 %, 109 us CPU/frame, 65 MB | 936 fps, SM 2 %, 87 us CPU/frame, 93 MB | 935 fps, SM 3 %, 102 us CPU/frame, 65 MB | 931 fps, SM 3 %, 158 us CPU/frame, 51 MB | - | - | - |
| `h264_1366x768p30.mkv` | 1669 fps, SM 3 %, 73 us CPU/frame, 35 MB | 1674 fps, SM 2 %, 64 us CPU/frame, 49 MB | 1672 fps, SM 3 %, 80 us CPU/frame, 35 MB | 1666 fps, SM 3 %, 150 us CPU/frame, 29 MB | - | - | - |
| `h264_480i.mkv` | 3284 fps, SM 3 %, 66 us CPU/frame, 12 MB | 3287 fps, SM 1 %, 56 us CPU/frame, 19 MB | 3290 fps, SM 3 %, 71 us CPU/frame, 12 MB | 3293 fps, SM 3 %, 98 us CPU/frame, 10 MB | 3280 fps, SM 26 %, 133 us CPU/frame, 16 MB | 6525 fps, SM 48 %, 90 us CPU/frame, 18 MB | 6559 fps, SM 7 %, 58 us CPU/frame, 11 MB |
| `h264_720p50.mkv` | 1808 fps, SM 3 %, 73 us CPU/frame, 35 MB | 1811 fps, SM 2 %, 63 us CPU/frame, 49 MB | 1810 fps, SM 3 %, 81 us CPU/frame, 35 MB | 1805 fps, SM 3 %, 99 us CPU/frame, 29 MB | - | - | - |
| `h264_720p60.mkv` | 1829 fps, SM 3 %, 74 us CPU/frame, 35 MB | 1834 fps, SM 2 %, 62 us CPU/frame, 49 MB | 1832 fps, SM 3 %, 80 us CPU/frame, 35 MB | 1827 fps, SM 3 %, 97 us CPU/frame, 29 MB | - | - | - |
| `h264_854x480p30.mkv` | 3167 fps, SM 3 %, 61 us CPU/frame, 14 MB | 3171 fps, SM 1 %, 52 us CPU/frame, 21 MB | 3170 fps, SM 3 %, 68 us CPU/frame, 14 MB | 3177 fps, SM 3 %, 131 us CPU/frame, 11 MB | - | - | - |
| `hevc_1080p30.mkv` | 2061 fps, SM 8 %, 91 us CPU/frame, 75 MB | 2087 fps, SM 5 %, 80 us CPU/frame, 103 MB | 2067 fps, SM 8 %, 94 us CPU/frame, 74 MB | 2083 fps, SM 7 %, 161 us CPU/frame, 75 MB | - | - | - |
| `hevc_1080p30_10.mkv` | 1876 fps, SM 15 %, 104 us CPU/frame, 121 MB | 1903 fps, SM 11 %, 82 us CPU/frame, 185 MB | 1881 fps, SM 15 %, 99 us CPU/frame, 121 MB | 1889 fps, SM 14 %, 178 us CPU/frame, 121 MB | - | - | - |
| `hevc_2160p24_10.mkv` | 471 fps, SM 26 %, 303 us CPU/frame, 431 MB | 495 fps, SM 13 %, 255 us CPU/frame, 623 MB | 471 fps, SM 25 %, 300 us CPU/frame, 431 MB | 464 fps, SM 28 %, 353 us CPU/frame, 407 MB | - | - | - |
| `hevc_2160p30.mkv` | 537 fps, SM 12 %, 250 us CPU/frame, 235 MB | 549 fps, SM 7 %, 194 us CPU/frame, 347 MB | 538 fps, SM 12 %, 227 us CPU/frame, 235 MB | 535 fps, SM 12 %, 366 us CPU/frame, 221 MB | - | - | - |
| `mpeg2_1080i.ts` | 865 fps, SM 3 %, 154 us CPU/frame, 42 MB | 872 fps, SM 2 %, 122 us CPU/frame, 57 MB | 868 fps, SM 3 %, 143 us CPU/frame, 42 MB | 871 fps, SM 3 %, 198 us CPU/frame, 45 MB | 847 fps, SM 35 %, 222 us CPU/frame, 79 MB | 1660 fps, SM 64 %, 136 us CPU/frame, 79 MB | 1721 fps, SM 5 %, 114 us CPU/frame, 45 MB |
| `mpeg2_480i.ts` | 3231 fps, SM 3 %, 77 us CPU/frame, 10 MB | 3231 fps, SM 2 %, 62 us CPU/frame, 13 MB | 3230 fps, SM 3 %, 81 us CPU/frame, 10 MB | 3255 fps, SM 3 %, 99 us CPU/frame, 10 MB | 3244 fps, SM 26 %, 123 us CPU/frame, 18 MB | 6459 fps, SM 48 %, 94 us CPU/frame, 16 MB | 6505 fps, SM 7 %, 58 us CPU/frame, 9 MB |
| `vp9_1080p30.mkv` | 1675 fps, SM 6 %, 110 us CPU/frame, 114 MB | 1671 fps, SM 3 %, 92 us CPU/frame, 153 MB | 1669 fps, SM 6 %, 108 us CPU/frame, 114 MB | 1681 fps, SM 6 %, 242 us CPU/frame, 113 MB | - | - | - |
