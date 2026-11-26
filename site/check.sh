#!/bin/sh
# Checks the site before it is published: every local link is relative and
# resolves, no resource comes from another host, no script, every image has
# alt, width and height, and the whole stays within its size budget.
# Usage: sh site/check.sh [dir]   (default: site). Exit 1 on any fault.
# POSIX sh with grep, sed, find, wc: CI's policy job installs nothing else.
# Attributes are read in double quotes only, as the page writes them.

dir=${1:-site}
faults=0

fault() {
	printf '%s\n' "$1"
	faults=$((faults + 1))
}

# Budgets, in bytes.
text_budget=$((120 * 1024))       # everything but images/
image_budget=$((400 * 1024))      # each image
images_budget=$((2560 * 1024))    # images/ in all

# One reference: refuse absolute paths, refuse another host's resource,
# resolve the rest against the referring file's folder.
check_ref() {
	file=$1 kind=$2 ref=$3
	case $ref in
	'#'* | mailto:* | data:*) return ;;
	http://* | https://*)
		[ "$kind" = href ] || fault "$file: external resource: $ref"
		return
		;;
	/*) fault "$file: absolute link: $ref"; return ;;
	esac
	path=${ref%%#*}
	path=${path%%\?*}
	[ -n "$path" ] || return
	base=$(dirname "$file")
	[ -e "$base/$path" ] || fault "$file: missing: $path"
}

for f in $(find "$dir" -type f -name '*.html'); do
	flat=$(tr '\n' ' ' <"$f")
	if printf '%s' "$flat" | grep -qi '<script'; then
		fault "$f: script"
	fi
	printf '%s' "$flat" | grep -o '<img[^>]*>' | while IFS= read -r img; do
		case $img in *' alt="'*) ;; *) echo "$f: img without alt/width/height: $img" ;; esac
		case $img in *' width="'*) ;; *) echo "$f: img without alt/width/height: $img" ;; esac
		case $img in *' height="'*) ;; *) echo "$f: img without alt/width/height: $img" ;; esac
	done | sort -u >"${TMPDIR:-/tmp}/site-check.$$"
	if [ -s "${TMPDIR:-/tmp}/site-check.$$" ]; then
		cat "${TMPDIR:-/tmp}/site-check.$$"
		faults=$((faults + $(wc -l <"${TMPDIR:-/tmp}/site-check.$$")))
	fi
	rm -f "${TMPDIR:-/tmp}/site-check.$$"
	for kind in href src; do
		for ref in $(printf '%s' "$flat" | grep -o " $kind=\"[^\"]*\"" | sed "s/^ $kind=\"//; s/\"\$//"); do
			check_ref "$f" "$kind" "$ref"
		done
	done
done

for f in $(find "$dir" -type f -name '*.css'); do
	for ref in $(grep -o 'url([^)]*)' "$f" | sed "s/^url(//; s/)\$//; s/^[\"']//; s/[\"']\$//"); do
		check_ref "$f" url "$ref"
	done
done

size() { wc -c <"$1" | tr -d ' '; }

text=0
for f in $(find "$dir" -type f ! -path "$dir/images/*"); do
	text=$((text + $(size "$f")))
done
[ "$text" -le "$text_budget" ] || fault "$dir: over budget: $((text / 1024)) KB without images (max $((text_budget / 1024)))"

images=0
if [ -d "$dir/images" ]; then
	for f in $(find "$dir/images" -type f); do
		s=$(size "$f")
		images=$((images + s))
		[ "$s" -le "$image_budget" ] || fault "$f: over budget: $((s / 1024)) KB (max $((image_budget / 1024)))"
	done
fi
[ "$images" -le "$images_budget" ] || fault "$dir/images: over budget: $((images / 1024)) KB (max $((images_budget / 1024)))"

if [ "$faults" -gt 0 ]; then
	exit 1
fi
echo "site: ok ($((text / 1024)) KB without images, $((images / 1024)) KB images)"
