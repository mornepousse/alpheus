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

# One reference: refuse absolute paths, refuse another host's resource (only
# a link a visitor follows, <a href>, may leave the site), refuse a path that
# climbs out of the site (only the site's folder is published), resolve the
# rest against the referring file's folder.
check_ref() {
	file=$1 kind=$2 ref=$3
	case $ref in
	'#'* | mailto:* | data:*) return ;;
	http://* | https://*)
		[ "$kind" = nav ] || fault "$file: external resource: $ref"
		return
		;;
	/*) fault "$file: absolute link: $ref"; return ;;
	esac
	path=${ref%%#*}
	path=${path%%\?*}
	[ -n "$path" ] || return
	case /$path/ in
	*/../*) fault "$file: leaves the site: $path"; return ;;
	esac
	base=$(dirname "$file")
	[ -e "$base/$path" ] || fault "$file: missing: $path"
}

for f in $(find "$dir" -type f -name '*.html'); do
	flat=$(tr '\n' ' ' <"$f")
	if printf '%s' "$flat" | grep -qi '<script'; then
		fault "$f: script"
	fi
	# Every tag, read in this shell (a here-document, not a pipe) so that
	# fault() counts.
	tags=$(printf '%s' "$flat" | grep -o '<[a-zA-Z][^>]*>')
	while IFS= read -r tag; do
		name=${tag#<}
		name=${name%%[ >]*}
		if [ "$name" = img ]; then
			for attr in alt width height; do
				case $tag in
				*" $attr=\""*) ;;
				*) fault "$f: img without alt/width/height: $tag"; break ;;
				esac
			done
		fi
		for attr in href src; do
			ref=$(printf '%s' "$tag" | grep -o " $attr=\"[^\"]*\"" | sed "s/^ $attr=\"//; s/\"\$//")
			[ -n "$ref" ] || continue
			kind=resource
			[ "$name" = a ] && [ "$attr" = href ] && kind=nav
			check_ref "$f" "$kind" "$ref"
		done
	done <<TAGS
$tags
TAGS
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
