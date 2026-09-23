#!/bin/bash
# snapshot a tree: every path with type, and sha256 for files
cd "$1" && find . -print0 | sort -z | while IFS= read -r -d '' p; do if [ -f "$p" ]; then echo "F $p $(shasum -a 256 "$p" | cut -c1-16)"; elif [ -d "$p" ]; then echo "D $p"; else echo "? $p"; fi; done
