

## P8 (compressed backups, design step)

All measurements are on a `cp -c` copy of `~/AgentKB/raw` kept outside the repository (deleted afterwards); the real directory was only read.

- `compress_bench.py <raw copy> <level> <out.tsv> [--verify]`: compress every plain file with the zstd CLI (single thread), one line per file. `c3.tsv`, `c9.tsv`, `c12.tsv`, `c19.tsv` are the outputs (relative path, bytes, compressed bytes, seconds; no session content).
- `thresholds.py <rows.txt> <bench.tsv>`: space saved per age threshold. `rows.txt` is `sqlite3 -separator '|' kb.db "select f.id,f.source,f.mirror_rel,f.mirror_offset,f.is_compressed,f.source_status,coalesce(s.last_activity_at,''),f.src_mtime_ns/1000000000 from files f left join sessions s on s.file_id=f.id"` (not committed: it lists session paths).
- `orphans.py <raw copy> <rows.txt>`: are the plain files no row points to byte-identical to the decompressed `.zst` next to them (result: 214 of 214 equal).
- `spike/`: `Cargo.toml.txt` and `main.rs.txt` of a throw-away program (`zstd` 0.13, `sha2`) that packs with a level, verifies by SHA-256, and times `read_line_at` / `decode_all` the way the app does. Rename to build it outside the repository.
