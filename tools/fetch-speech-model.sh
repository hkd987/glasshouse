#!/usr/bin/env bash
# Usage: tools/fetch-speech-model.sh [--check]
# The speech model Meet turns its person's words into text with, on the computer (ADR 0045 T2):
# Moonshine tiny (English, MIT), quantized for ONNX Runtime Web, and the licenses of it and of
# the runtime. Each file at a pinned revision, checked against its SHA-256, into
# build/cache/models/; verified files are stamped and skipped on later runs. Once all are there
# it lists them in build/cache/models/speech.json (where Meet ships each one: name -> file),
# which `make apps-build` (apps/build.mjs) copies into Meet; a run that fails leaves no list, and
# Meet then ships without the model. --check only says whether they're all there (exit 1 if not).
set -euo pipefail

REPO=$(cd "$(dirname "$0")/.." && pwd)
CACHE=${SPEECH_CACHE:-$REPO/build/cache/models}

MODEL_REV=a6da1241cd305dcd64eab1edbd615f2bb9aabb95
MODEL_URL=https://huggingface.co/onnx-community/moonshine-tiny-ONNX/resolve/$MODEL_REV
MOONSHINE_REV=234f60faa0eb388b01cdf7e60aca232af37aefda
# onnxruntime-web's version in apps/package-lock.json: tag v1.30.0.
ORT_REV=f2c39fe2f838cf35ce7da92824f5a5e3ee6e88a7

# <cache path> <url> <sha256> <where Meet ships it>
FILES=(
	"moonshine-tiny/$MODEL_REV/onnx/encoder_model_quantized.onnx"
	"$MODEL_URL/onnx/encoder_model_quantized.onnx"
	c6fc4b7bc5af75c0591fd157a1f3829b533d18e9769a888fd95a62e470dd4f4a
	speech/encoder.onnx

	# The decoder that keeps what it worked out for earlier tokens: twice as fast.
	"moonshine-tiny/$MODEL_REV/onnx/decoder_model_merged_quantized.onnx"
	"$MODEL_URL/onnx/decoder_model_merged_quantized.onnx"
	eed87831c3a6103534aae7d47a5d485025c659a1323901513961c39fe8a1a367
	speech/decoder.onnx

	"moonshine-tiny/$MODEL_REV/tokenizer.json"
	"$MODEL_URL/tokenizer.json"
	7b913404bdd039af4756783218af4440bc07fb7d6d8258d677e34f95b3ec416f
	# Meet ships its tokens alone (speech/tokens.json), a tenth of the size.
	speech/tokenizer.json

	"moonshine-tiny/$MODEL_REV/LICENSE-moonshine.txt"
	"https://raw.githubusercontent.com/moonshine-ai/moonshine/$MOONSHINE_REV/LICENSE"
	fa7d1174dd8af6a7cd280be20b80d10095ed4c19b5b20b61a7715c3ad790dc5f
	speech/LICENSE-moonshine.txt

	"onnxruntime-$ORT_REV/LICENSE-onnxruntime.txt"
	"https://raw.githubusercontent.com/microsoft/onnxruntime/$ORT_REV/LICENSE"
	2f07c72751aed99790b8a4869cf2311df85a860b22ded05fa22803587a48922c
	ort/LICENSE-onnxruntime.txt

	"onnxruntime-$ORT_REV/NOTICE-onnxruntime.txt"
	"https://raw.githubusercontent.com/microsoft/onnxruntime/$ORT_REV/ThirdPartyNotices.txt"
	143764b952fdb1a7c69ce653bfba74a7744d6a8a573bfb73e235fba356c83de3
	ort/NOTICE-onnxruntime.txt
)

check=false
[[ ${1:-} == --check ]] && check=true

list=$CACHE/speech.json
$check || rm -f "$list"
missing=0
for ((i = 0; i < ${#FILES[@]}; i += 4)); do
	dest=$CACHE/${FILES[i]} url=${FILES[i + 1]} sum=${FILES[i + 2]}
	[[ -f $dest && $(cat "$dest.sha256-ok" 2>/dev/null) == "$sum" ]] && continue
	if $check; then
		echo "missing: ${dest#"$REPO"/}"
		missing=1
		continue
	fi
	mkdir -p "$(dirname "$dest")"
	echo "fetching ${dest#"$CACHE"/}"
	curl -fsSL --retry 3 --retry-delay 2 -o "$dest.part" "$url"
	if ! echo "$sum  $dest.part" | sha256sum -c --quiet - >/dev/null 2>&1; then
		rm -f "$dest.part"
		echo "${url} isn't the file pinned here (SHA-256 differs)" >&2
		exit 1
	fi
	mv "$dest.part" "$dest"
	echo "$sum" >"$dest.sha256-ok"
done
$check && exit "$missing"

{
	echo "{"
	for ((i = 0; i < ${#FILES[@]}; i += 4)); do
		((i + 4 < ${#FILES[@]})) && comma=, || comma=
		printf '  "%s": "%s"%s\n' "${FILES[i + 3]}" "$CACHE/${FILES[i]}" "$comma"
	done
	echo "}"
} >"$list.part"
mv "$list.part" "$list"
