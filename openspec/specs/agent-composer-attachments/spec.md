# agent-composer-attachments Specification

## Purpose
Let an Agent conversation carry images: pasting one into the composer attaches
it to the pending message, the composer shows what is attached and lets it be
taken back off, and a sent message shows its images in the transcript.
## Requirements
### Requirement: Attach a pasted image to the pending message
NiumaTerm SHALL attach an image pasted into the Agent composer to the pending
message and insert a placeholder of the form `[Image #N]` at the caret, where
`N` is the attachment's position in the pending list counting from 1. A paste
that carries no image MUST paste as text, unchanged.

#### Scenario: Paste an image into an empty composer
- **WHEN** the clipboard holds a screenshot and the user pastes into an empty Agent composer
- **THEN** the composer text becomes `[Image #1]` and the pending message has one attachment

#### Scenario: Paste an image mid-sentence
- **WHEN** the composer holds `look at ` with the caret at the end and the user pastes an image
- **THEN** the composer text becomes `look at [Image #1]` and the caret sits after the placeholder

#### Scenario: Paste a second image
- **WHEN** one image is already attached and the user pastes another
- **THEN** the new placeholder reads `[Image #2]` and the pending message has two attachments

#### Scenario: Paste text
- **WHEN** the clipboard holds text and the user pastes
- **THEN** the text is inserted and no attachment is created

### Requirement: Show pending attachments above the composer
NiumaTerm SHALL show every pending attachment as a thumbnail above the Agent
composer, in placeholder order, each with a control that removes it. The strip
MUST NOT occupy space while nothing is attached.

#### Scenario: Strip appears with the first attachment
- **WHEN** an image is attached to an empty composer
- **THEN** a strip above the composer shows one thumbnail of that image

#### Scenario: Strip is absent with nothing attached
- **WHEN** the pending message has no attachments
- **THEN** no strip is shown and the composer keeps its usual position

#### Scenario: Thumbnails follow placeholder order
- **WHEN** three images are attached
- **THEN** the strip shows their thumbnails in the order `[Image #1]`, `[Image #2]`, `[Image #3]`

### Requirement: Remove an attachment from the strip
NiumaTerm SHALL, when an attachment's remove control is used, drop that
attachment, delete its placeholder from the composer text, and renumber the
remaining placeholders so they stay consecutive from 1 in both the text and
the strip. Text the user wrote around the deleted placeholder MUST be
preserved.

#### Scenario: Remove the only attachment
- **WHEN** the composer reads `look at [Image #1] please` and that attachment is removed
- **THEN** the composer reads `look at  please`, the strip is gone, and the pending message has no attachments

#### Scenario: Remove a middle attachment
- **WHEN** the composer reads `[Image #1] and [Image #2] and [Image #3]` and the second attachment is removed
- **THEN** the composer reads `[Image #1] and  and [Image #2]` and the strip shows the first and third images in that order

### Requirement: Keep the strip and the composer text in agreement
NiumaTerm SHALL drop an attachment whose placeholder no longer appears in the
composer text, and MUST renumber the remaining placeholders so they stay
consecutive from 1. A message MUST NOT be sent with an attachment the text
does not name.

#### Scenario: Delete a placeholder by editing
- **WHEN** the composer reads `[Image #1] and [Image #2]` and the user deletes the `[Image #1]` text by hand
- **THEN** the first attachment is dropped and the composer reads ` and [Image #1]` with one thumbnail in the strip

#### Scenario: Clear the composer
- **WHEN** the composer holds placeholders and the user selects all and deletes
- **THEN** every attachment is dropped and the strip is gone

### Requirement: Send attachments with the message
NiumaTerm SHALL deliver a sent message's attachments to the harness alongside
its text, in placeholder order, and MUST clear the pending attachments once the
message is accepted. A submission the harness rejects MUST keep its attachments
pending, so the message stays recoverable.

#### Scenario: Send a message with images
- **WHEN** a message reading `compare [Image #1] with [Image #2]` is accepted
- **THEN** the harness receives both images with that text, in that order, and the strip is empty

#### Scenario: A rejected submission keeps its attachments
- **WHEN** a message with an attachment is refused because the session is not ready
- **THEN** the attachment stays pending and the strip still shows its thumbnail

#### Scenario: Text-only messages are unaffected
- **WHEN** a message with no attachments is sent
- **THEN** the harness receives exactly what it received before this capability existed

### Requirement: Show a sent message's images in the transcript
NiumaTerm SHALL render the images of a sent user message as thumbnails in the
transcript, so a reader sees what was sent rather than only the placeholder
text.

#### Scenario: A sent message shows its image
- **WHEN** a message with one attachment appears in the transcript
- **THEN** its thumbnail is shown with the message text

#### Scenario: A resumed conversation
- **WHEN** a conversation is resumed and its replay contains a message that carried an image
- **THEN** the message renders without error, whether or not the image itself is still available

### Requirement: Bound what a message may carry
NiumaTerm SHALL attach at most 8 images to one message and MUST tell the user
when a paste is refused for exceeding that. An image larger than 2048 pixels on
its long edge MUST be downscaled, preserving its aspect ratio, before it is
attached.

#### Scenario: Refuse a ninth image
- **WHEN** eight images are attached and the user pastes another
- **THEN** the paste is refused, the composer text is unchanged, and the user is told why

#### Scenario: Downscale an oversized image
- **WHEN** a 3840x2160 screenshot is pasted
- **THEN** the attachment is at most 2048 pixels on its long edge and keeps its 16:9 shape

#### Scenario: Leave a small image alone
- **WHEN** a 800x600 image is pasted
- **THEN** the attachment keeps its original dimensions

