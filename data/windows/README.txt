Slerp for Windows
=================

Thumbnails and a viewer for game and 3D asset files: KTX2, DDS, glTF, FBX,
USD, Quake and Doom files, materials and more (README.md lists them all).

  slerp.exe        the command line and the viewer
  slerpw.exe       opens files in the viewer without a console window
  slerp_shell.dll  Explorer's thumbnail handler

Keep the three files together in a folder that stays put, then run this
once in a terminal there (no administrator rights needed):

  slerp.exe register

Explorer then shows thumbnails of these files and offers Slerp under
"Open with".  Explorer windows that are already open may need a refresh
(F5).  Run it again after moving the folder.  Which file types get Slerp's
thumbnails, and which open in Slerp, can be chosen in the viewer's
Settings > File Types or with

  slerp.exe associate --list
  slerp.exe associate --thumbnails off glb      (or on; formats, categories or all)
  slerp.exe associate --default on glb          (opens Windows' Default apps page)

Windows lets only its own Settings choose default apps, so Slerp opens that
page for you to pick it.  To remove Slerp, run

  slerp.exe unregister

before deleting the folder; it also puts back any thumbnail handler Slerp
replaced.  Explorer keeps slerp_shell.dll loaded, so
to replace it with a newer version, first restart Explorer (Task Manager,
Windows Explorer, Restart) or sign out and back in.  The command line:

  slerp.exe view [file]
  slerp.exe thumbnail 256 <file> <out.png>
  slerp.exe render <file> <out.png> [--size 512]
  slerp.exe info <file>

Settings live in %APPDATA%\Slerp\config.jsonc; README.md describes them.
