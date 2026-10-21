# Place names for encrypted worlds

Sakhal's and Nasdara's world files are encrypted (`worlds_*.ebo`), so the importer can't read
their place names. `src/import/places/` keeps a copy of them, taken from the game itself with
this offline mission. Run it again when an update moves or renames places.

1. Close DayZ. Steam must be running and signed in to an account that owns DayZ.
2. Start `DayZDiag_x64.exe` (in the DayZ folder) offline with this mission:

   ```
   DayZDiag_x64.exe -mission=<path>\dumpplaces.chernarusplus -profiles=<output folder> -window -nosplash
   ```

   On Linux, run it through Proton with the game's prefix (`compatdata/221100`), inside the
   Steam Linux Runtime, as Steam would. The game loads Chernarus, writes the file and quits.
3. `<output folder>\placenames.json` has every world's `Names` list. Copy the `Sakhal` and
   `Nasdara` arrays into `src/import/places/sakhal.json` and `nasdara.json`, and bump
   `POI_VERSION` in `src/import/catalog.rs` so existing imports pick them up.

The names are the config's own: Sakhal's are Russian, which the importer transliterates as it
does Chernarus's.
