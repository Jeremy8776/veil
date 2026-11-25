# Veil

Peer-to-peer video calling. These are the words Veil uses for what a person sees and sends in a call.

## Video effects

**Effect**:
A change Veil makes to this device's camera picture before anyone sees it, including the person's own self-view.
_Avoid_: Processing, enhancement

**Filter**:
An Effect that changes the colour or tone of the whole picture, such as brightness, warmth or black-and-white.
_Avoid_: Lens, AR effect, face effect

**LUT** (planned, FX-01):
A colour look-up table file (`.cube`) that the person imports and uses as a Filter.
_Avoid_: Preset, profile, grade (alone)

**Strength** (planned, FX-01):
How much of an Effect is applied, from 0% (the original picture) to 100% (the full Effect).
_Avoid_: Intensity, amount, opacity

**Background blur**:
An Effect that blurs everything in the picture except the person in front of the camera.
_Avoid_: Portrait mode, bokeh, blur (alone)

**Blur add-on**:
An optional download, made only when the person asks for it, that lets Background blur trace the person's outline more precisely. Veil works fully without it.
_Avoid_: Plugin, pack, premium, model (alone), HD blur

## Screen sharing

**Share picker**:
Veil's own window for choosing what to share and which part of it.
_Avoid_: Source list, share dialog

**System chooser**:
The desktop's own dialog for choosing a screen or window to share, used where Veil is not allowed to list them itself. Veil only learns what the person picked there.
_Avoid_: Portal, permission prompt

**Computer sound** (planned, SHARE-01):
The sound a device is playing, which the person can choose to send with a screen share. It never includes Veil's own call audio.
_Avoid_: System audio, desktop audio, loopback
