# Cuts the first part of a very large ASCII DXF at an entity boundary, to make a drawing of a chosen size from a
# real one.  Used for local/assets/cad-extra/dxf/bogota/Licencia_excerpt.dxf, the first 22 MB of "Licencia.dxf"
# (268 MB, exported by ArcGIS 10.8, R14, CRLF line ends, only LWPOLYLINEs) from the Bogota open-data portal
# (Secretaria Distrital de Planeacion, CC-BY-4.0).  The excerpt keeps the header, tables and blocks, the first
# LWPOLYLINEs, and the OBJECTS section from the end of the original, so it stays a valid drawing.
#
# Run (Python 3.8+, standard library only):
#   python excerpt-large-dxf.py Licencia.dxf Licencia_excerpt.dxf 22000000
# The last argument is the approximate size in bytes.  Entity lines must end in CRLF ("  0\r\nLWPOLYLINE\r\n").

import sys

src, out, n = sys.argv[1], sys.argv[2], int(sys.argv[3])
data = open(src, "rb").read()
cut = data.rindex(b"  0\r\nLWPOLYLINE\r\n", 0, n)
objects = data.rindex(b"  0\r\nSECTION\r\n  2\r\nOBJECTS\r\n")
result = data[:cut] + b"  0\r\nENDSEC\r\n" + data[objects:]
open(out, "wb").write(result)
print(f"{len(result):,} bytes: entities cut at byte {cut:,}, OBJECTS section copied from byte {objects:,}")
