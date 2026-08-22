# Local draft input preview

Use `flea draft preview` to normalize and validate listing input before creating
remote state:

```sh
flea draft preview \
  --category 258 \
  --title "Koivutuoli" \
  --description "Hyväkuntoinen tuoli noudettavaksi Helsingistä." \
  --price 45.50 \
  --trade-type sell \
  --postal-code 00100 \
  --delivery pickup \
  --image tuoli.heic
```

The command runs offline by default and does not require authentication. It
checks JSON shape and duplicate inputs, generic text limits, price syntax,
Finnish postal-code shape, delivery values, image files, supported formats,
dimensions, conversion, and metadata removal. It reports normalized listing
fields, a local image-processing plan, assumptions, requirements that remain
unverifiable, and a bounded `listing.json` representation for `draft create`.
Image paths in that representation contain file names only.

Preview never creates a draft or uploads an image. A successful preview means
that the input passed Flea's local checks. It does not mean that Tori will accept
or publish the listing.

## Optional composer fields

Condition has a dedicated flag that accepts a composer machine value:

```sh
flea draft create --category 46 --condition 2 --input listing.json
```

Discover the valid value from an existing draft before setting it:

```sh
flea draft show DRAFT_ID --include-options condition
```

Other category-specific optional fields use the bounded `attributes` namespace
in JSON input:

```json
{
  "attributes": {
    "material": "10"
  }
}
```

Flea applies category first, refreshes the composer, and requires every
attribute key to name an optional composer field with a supported type. Select
values must match that composer's machine values. Use JSON `null` to clear a
persisted optional field. Local preview preserves these inputs and identifies
composer validation as unverifiable because it has no draft model.

## Read-only category enrichment

Add `--verify-category` to query the authenticated category taxonomy:

```sh
flea draft preview --input listing.json --verify-category
```

This mode makes one read-only taxonomy request. Output separates local
validation from the remotely verified category existence and selectability
constraints. Category-specific fields and options still require an authoritative
remote draft model and remain listed as unverifiable.

## Image privacy

JPEG and PNG images are decoded and re-encoded locally so EXIF, GPS, XMP,
embedded thumbnails, and other source metadata are absent from prepared upload
bytes. HEIC and HEIF images are converted to JPEG and then re-encoded through
the same metadata-stripping path. Flea uses macOS ImageIO through `sips` on
macOS and falls back to the optional `heif-convert` command on other platforms.
A missing decoder produces an actionable local validation error.

Conversion artifacts live in a private temporary directory that is removed on
success and failure. Original files are read only. Preview output reports image
format, final dimensions, byte size, and metadata-stripping status without
printing source metadata or directory paths.

## Preview versus remote validation

`flea draft preview` validates caller input before a remote draft exists. Its
text and image checks are local, taxonomy enrichment is optional and read only,
and its output always lists assumptions and unverifiable publication rules.

`flea draft validate DRAFT_ID` is the authoritative read-only check for an
existing remote draft. It evaluates the persisted draft, composer model,
category-specific requirements, delivery configuration, and remote image
processing state. Use remote validation after creation and before considering a
draft ready for publication.
