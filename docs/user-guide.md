# stuttter user guide

stuttter is a stereo audio effect for Bitwig: it chops, repeats, reverses
and tape-stops whatever a track feeds it, in time with the song. It does that
by recording everything that comes in and, for every sample it sends out,
choosing how far back in that recording to read. It never reads ahead, which
is why it adds no latency.

**This version builds the Time row, the Volume row and the waveform view.**
The playing side is finished — the actions below all work, they follow the
transport, and the Pattern parameter or notes switch between them. In the
window you can pick which pattern you are looking at, set its **rate** and
**length**, build its Time row — click an empty step to put a block on it,
drag the block to move or stretch it, select it to set what it does in the
block panel (a preset, its loop, where it reads from and its speed),
duplicate it or remove it — draw its step levels by dragging across the
Volume row, watch what the effect is doing to the sound in the waveform view,
and choose whether the window stays above Bitwig's.
Everything this guide says about how a pattern *plays* is already true and
will stay true.

## Putting it on a track

stuttter is an audio effect, so it goes in a track's device chain after
whatever makes the sound — on the track itself, or in an FX track you send
to. It sends no notes, so nothing after it on the track needs to be rewired
around it. It can take notes, but only to play its patterns, and only once
you switch that on — see
[Playing patterns from notes](#playing-patterns-from-notes).

It never asks Bitwig to delay the track to make room for it, so putting it on
a track does not push that track out of time with the others.

## The three parameters

These are Bitwig parameters, so each one can be automated, driven by a
modulator, mapped to a controller, and is saved with the project.

| Parameter | Range | Default | What it is |
|---|---|---|---|
| **Pattern** | **Off**, then 1 to 16 | 1 | Which of the sixteen patterns plays. **Off** means the input is passed through untouched. Seventeen fixed positions, so a controller or an automation lane steps between whole patterns rather than sliding across them. Not used while **Notes** is on — then the notes decide. |
| **Mix** | 0 % to 100 % | 100 % | How much of the effect you hear against the untouched input. At 0 % you hear the input alone, sample for sample. |
| **Smoothing** | 0.5 ms to 20 ms | 3.0 ms | The length of the short crossfade used wherever the effect would otherwise jump and click. Short values are sharper, long ones softer. |

Set them the way you set any Bitwig parameter: on the device panel, or by
typing a value into Bitwig's own field for it. **Pattern** reads out as `Off`
at its lowest position, and typing `off` back into it selects that position
again.

## What a pattern plays

A pattern is a row of steps — 16 of them by default, up to 64 — each one a
note value long: **1/8**, **1/8T**, **1/16**, **1/16T** or **1/32**. Sixteen
1/16 steps fill one 4/4 bar exactly.

The steps are counted from the start of the song, not from the moment you
switched the pattern on, so a pattern always sits square with the bar: step 1
of a 16-step 1/16 pattern lands on every downbeat, whenever you bring it in.

Over those steps a pattern holds:

- a **Time row** of blocks, each covering one or more steps and carrying a
  recipe for what the plugin does with time while that block plays;
- a **Volume row**, one level per step, which multiplies the effect signal;
- a **filter** — off, or a low-, high- or band-pass — with a **Cutoff row**
  and a **Res row** setting its cutoff and resonance step by step.

Steps with no block over them play live: the input comes straight through.

### What a block does

Every block is a rule for how far behind the live input to read, so all of
them play back audio the track has already made. A block has three settings:

- **Loop** — off, or a length from **1/1** (a whole bar) down to **1/64**,
  plus triplets. With a loop, the block keeps going back to where it started
  every loop length: that is the stutter. Without one, it plays straight on.
- **Source** — where in the past the block starts reading:
  - *Block start*: at the block's own start. The first time round a loop
    plays what would have played live, then it repeats.
  - *Fixed spot*: a step of the pattern you choose, by dragging the block's
    box on the waveform or in the panel. A loop is then heard from the
    block's very first sample, because it is repeating audio that has
    already gone by. The spot stays put when you move the block, so you can
    take a beat from the start of the pattern and bring it back near the
    end without covering the steps in between. A spot at or after the
    block's own start is taken from the pass before (the future has not been
    recorded yet), and you can reach up to 8 more passes back.
  - *Continue previous block*: wherever the block just before this one left
    off. This is how you chain blocks: three repeats of a beat followed by a
    tape stop set to continue slows down the fourth time round of that same
    beat. If no block ends exactly where this one starts, it reads from its
    own start instead. The first block of a pattern continues the last one
    when that one ends on the pattern's last step.
- **Speed** — the read speed at the block's start and at its end, from −2 to
  +2, changing smoothly in between. **1** is normal, **2** an octave up,
  **0.5** an octave down, **0** frozen, **−1** backwards. Pitch follows
  speed, the way it does on tape; there is no separate pitch control. On a
  looping block every repeat plays at the speed of its moment in the block,
  so a loop going from 1 to 2 is a stutter that rises an octave.

A block never reads the future. If its settings would get ahead of the live
input — a loop played faster than normal from the block's own start, say —
it starts reading just far enough back to stay behind it.

#### The presets

Six one-click presets fill in the loop and speed for you (they keep the
source you chose):

| Preset | What you hear |
|---|---|
| **Repeat** | The classic stutter: a loop of one step, at normal speed. |
| **Half speed** | Plays on at half speed an octave down, falling behind as it goes. |
| **Double speed** | Plays at double speed an octave up; from the block start it arrives back exactly on the live input as the block ends, so the next step joins seamlessly. |
| **Reverse** | Turns round and runs backwards at pitch through the audio before it. |
| **Tape stop** | Starts at normal speed and slows smoothly to a standstill, pitch falling with it. |
| **Tape start** | Starts from a standstill and speeds smoothly up to normal, pitch rising with it. |

For example, with a snare on beats 2 and 4: a Repeat with a **1/8** loop over
two beats, then a Repeat with a **1/16** loop and speed **1 → 2** over the
next two beats, gives a stutter that doubles in rate and rises in pitch.

### The Volume row

Each step carries its own level, from silence to full, and it applies to the
effect signal only. Level changes between steps are ramped over the
**Smoothing** time rather than switched, so a gate pattern does not click.
Every step starts at full level, so a pattern with nothing drawn in this row
changes no volumes. **Drawing the Volume row** below says how to draw them.

### The filter

Each pattern has a filter that works like the Volume row: it acts on the
effect signal, only while the pattern plays, and every change is ramped over
the **Smoothing** time — a step to a new cutoff, the pattern starting or
stopping, even a change of filter type — so it never clicks. When the
pattern stops (Pattern set to Off, no note playing, or the transport stopped)
the filter fades out and the track comes back untouched.

| Filter | What it does |
|---|---|
| **Off** | No filter — the default, so a pattern you have not set up sounds as it always did. |
| **Low-pass** | Cuts above the cutoff: lower is darker and more muffled. |
| **High-pass** | Cuts below the cutoff: higher is thinner. |
| **Band-pass** | Keeps a band around the cutoff and cuts either side. |

The **Cutoff row** runs from 20 Hz at the bottom to 20 kHz at the top, evenly
in octaves. The **Res row** is the resonance: none at the bottom, a strong
peak at the cutoff at the top. Like the Volume row they belong to the step,
not to a block, so a step with no block over it is filtered too. The filter
comes before the Volume row's level, so a gate drawn on the Volume row stays
sharp. **Drawing the Volume row** below says how to draw all three rows.
### Why nothing clicks

Wherever the read position jumps — a block starting or ending, a repeat
wrapping round, a pattern change, the transport moving — the old read keeps
going at its own speed and fades out while the new one fades in, over the
**Smoothing** time. A jump arriving during a fade restarts the fade from
wherever it had got to, so a burst of them does not pile up.

## Switching patterns while the song plays

The **Pattern** parameter is the live performance control, and it is built to
be moved while the music runs — by hand, from a controller, or from an
automation lane in the arranger.

- A change **waits for the end of the step that is playing**, so it never
  cuts a repeat a third of the way through a slice or abandons a tape stop
  half way down. With 1/16 steps at 120 BPM that is at most an eighth of a
  second of waiting.
- Switching **away from Off** does not wait: Off has no steps running, so the
  pattern joins at once, at the current place in the bar.
- Switching **to Off** does wait — the step it interrupts is played out
  first, then the bypass starts on the boundary.
- Pressing play does not cost a step: the first block after the transport
  starts uses the pattern the parameter is on right then.

Automation written for this parameter behaves like automation for any stepped
parameter: the lane holds whole positions, and Bitwig draws it as steps
rather than a slope.

### Playing patterns from notes

For placing stutters at exact moments, notes are easier than automation:
each pattern gets its own key, and a note on that key plays the pattern.
Drawing, moving and stretching stutters is then the same as editing notes in
a clip.

Notes are **off** on a new device, because a device placed after an
instrument receives that instrument's notes too — after a Drum Machine every
kick and snare would switch patterns. Turn them on with **Notes** at the end
of the bar's second row, and pick where the sixteen keys start:

| Notes setting | Pattern 1 | Pattern 2 | … | Pattern 16 |
|---|---|---|---|---|
| C6–D#7 | C6 | C#6 | … | D#7 |
| C1–D#2 | C1 | C#1 | … | D#2 |

Any C from C-2 to C7 can be the start (Bitwig's note names, where middle C is
C3). Hover a pattern button to see the key that plays it.

**With Notes on, the notes are in charge.** Where no note is playing a
pattern, nothing plays and the track passes through untouched — the
**Pattern** parameter is not used at all, so there is nothing to switch off
between stutters. Turn Notes off to hand control back to the parameter.

**Held or latched.** The **Latch** button after Notes picks how a note plays
its pattern:

- **Latch off** (the default): a pattern plays for **exactly as long as its
  note lasts**. A long note is a long stutter. This suits stutter notes drawn
  in a clip.
- **Latch on**: a note **starts** its pattern and it keeps playing after the
  note ends, until the next pattern note — so a short drum hit or a click on
  a pad is enough. The key right after the sixteen (**E7** from C6) is
  **Stop**. Stopping the transport stops it too, so pressing play always
  starts clean; a pattern does keep going round a loop until a note changes
  it.

**Stutter notes on their own track (in the arranger).** Keep the drums as
they are and put the stutter notes on a track of their own:

1. Make an instrument track with nothing on it — call it **Stutter**. In
   the arranger, draw clips on it with notes from C6 up wherever you want a
   stutter: C6 for pattern 1, C#6 for pattern 2, and so on.
2. On the drum track, after the instrument, add a **Note Receiver** with its
   source set to the Stutter track, and put stuttter after it:
   instrument → Note Receiver → stuttter.
3. In stuttter, set **Notes** to **C6–D#7**.

The drum notes still pass along the chain to stuttter, which is why the
stutter keys sit above them at C6. If the Note Receiver does not offer the
Stutter track, check that the Stutter track's output is not routed into the
drum track — Bitwig hides sources that would loop back.

**Or in the same clip.** After a Drum Machine, C6–D#7 is above every pad, so
stutter notes can also go straight into the drum clip: the Drum Machine has
no sound on those keys, so they are silent and only steer stuttter.

How the notes play:

- A pattern **starts on the note** — on the exact sample, without waiting
  for a step to end the way the Pattern parameter does.
- It stays **in time with the song**, like everything else here: a note on
  beat 3 plays the part of the pattern that falls on beat 3, not the
  pattern's first step. A note where the pattern starts — on the bar line for
  a one-bar pattern — plays it from the top.
- With Latch off and **several notes held**, the newest one plays; when it
  ends, the one held before it takes over again.
- With **no note playing a pattern**, nothing plays.
- Velocity does nothing, and other keys are ignored.
- The pattern a note is playing gets the lit bar in the window, and with
  **Follow** on the window switches to it.
- Notes and Latch are saved with the project. Changing either lets go of
  any note playing.

## Following the transport

- **Stopped.** With the transport stopped there is no song position to place
  a pattern against, so the plugin plays live — and if a read was offset when
  you stopped, it crossfades back to live rather than cutting. Recording
  carries on while stopped, so a repeat right after you press play already
  has the audio from just before it.
- **Looping, and jumping about.** A loop that comes round, or a click into
  another part of the song, moves the read position; both are crossfaded like
  any other jump. A loop whose length happens to be a whole number of pattern
  cycles leaves the read position where it already was, and that case needs
  no fade at all.
- **Odd transport readings.** If Bitwig reports no position, no tempo, or a
  tempo outside 1–1000 BPM, that block counts as stopped and plays live.

## How far back it can reach

The plugin keeps the last **64 seconds** of input — sixteen bars of 4/4 at
60 BPM, which covers what any block normally needs. A block that does reach
further back than that (a long reverse on a 64-step 1/8 pattern at a slow
tempo, or a long chain of blocks each continuing the one before) fades out
smoothly over the **Smoothing** time where the recording runs out, rather
than clicking, and comes back when the read position does.

The same goes for the first moments after the plugin loads, and after a
project opens: there is nothing recorded yet, so a block that reaches
further back than the plugin has been running plays silence until it has
caught up.

## Tempo changes

Offsets are kept in beats and turned into samples with the tempo of the block
being played. While the tempo is *actually changing* — a tempo ramp, or a
tempo knob being moved — a held offset therefore slides by a little, so a
long repeat can drift a few milliseconds during the ramp. It is back in step
as soon as the tempo settles, and at a constant tempo, however fast or slow,
nothing slides. Exact behaviour under tempo automation is on the list for a
later version.

## Remote controls

The device has one remote-controls page, **Main**, in a section called
*stuttter*. It holds **Pattern**, **Mix** and **Smoothing** in that order,
so those three land on the first three knobs when Bitwig maps the device to a
controller. The other five knobs on the page are empty.

The patterns themselves will never appear here — a grid of steps is not
something a host can hold as parameters, and it is saved with the project
instead.

## The window

Click the device's plugin-window button to open it. It opens at 900 × 540
points. To resize it, drag the edge of Bitwig's plugin window, or the
**grip in its bottom-right corner** (the small diagonal lines). It goes down
to a minimum size that still fits every control. A size you drag it to is kept for as long as the project is open;
reopening the project opens it at 900 × 540 again.

Inside it:

- **A bar across the top**, two rows of controls: the pattern buttons on the
  first, and on the second the pattern's own settings followed by Mix,
  Smoothing, the window's Undo and Redo, and the **On top** switch.
- **On the left**, from the top: the **Time row**, the pattern's grid of
  steps with its blocks; a strip of three tabs (**Volume**, **Cutoff**,
  **Res**) with the pattern's **Filter** beside them; the row the tabs pick —
  the Volume row unless you choose another; and the **waveform view**, the sound itself — what the plugin heard and
  what it played, over one pass of the pattern. These three share their
  columns, so a step, its level and its sound are always above one another,
  and they grow with the window.
- **On the right, the block panel**: everything about the block you have
  selected on the Time row, or a reminder of the gestures when none is.

Rest the pointer on a control for a little over a second and it says what it
does. Moving across the window does not pop anything up.

### Choosing the pattern

The bar starts with sixteen numbered buttons, one per pattern:

- **Click a number** to edit that pattern **and play it**. The window shows
  it straight away; the sound switches at the next step, the same way a move
  of the **Pattern** parameter does — in fact the click *is* a move of that
  parameter, so Bitwig records it if you are recording automation. With
  **Notes** on, the notes decide what plays, so a click only opens the
  pattern for editing
  ([Playing patterns from notes](#playing-patterns-from-notes)).
- **Shift-click a number** to edit that pattern **without** playing it — to
  prepare the next pattern while another one plays.
- The pattern **that is playing** has a lit blue bar under its number —
  the one the Pattern parameter is on, or with Notes on the one a note is
  playing. When nothing plays, no button is lit.
- The pattern **you are editing** is the button that is filled in. Normally
  both marks are on the same button.
- **Follow**, the button after the numbers, decides what happens when the
  pattern changes by itself — automation, a controller, the device panel or
  a note. **Off** (the default), the window stays on the pattern you are
  editing, so you can work on it while the song plays and switches patterns
  around it. **On**, the window switches to whatever plays. Shift-clicking a
  number turns Follow off.

A window opens on the pattern that is playing, with Follow off.

**Right-click a number** for the three things you can do to a whole pattern
at once. The menu names the pattern it opened over, so you can always see which
one you are about to change:

- **Copy to…** opens a little block of the sixteen numbers. Pick one and that
  pattern is replaced by a copy of this one — its rate, its length, its
  blocks and its levels, all of it. The number you right-clicked is greyed
  out in the block, since copying a pattern over itself would do nothing.
- **Fill with…** lists ready-made patterns to start from. Pick one and what
  is drawn on this pattern is replaced by it — blocks, levels and filter —
  while its **rate and length stay as you set them**: the starter is laid
  out on your grid, however many bars it is. Point at one to read what it
  does. They are written for a 4/4 beat with the kick on 1 and 3 and the
  snare on 2 and 4:

  | Starter | What it does |
  | --- | --- |
  | Stutter build | Across the whole pattern: the first beat repeats over the first half, then repeats of 1/8, 1/16 and 1/32, each faster than the last. |
  | Tape stop ending | The pattern plays untouched, then a tape stop slows its last beat to a halt. |
  | Reverse snares | Beats 2 and 4 of every bar play backwards, taken from the bar before. |
  | Half-time | Every half bar plays its first beat at half speed, an octave down. |
  | Gate | The volume chops on and off every other step. |
  | Filter sweep | A low-pass filter closes from fully open to about 220 Hz across the whole pattern. |

  Change anything you like afterwards; a starter is an ordinary pattern once
  it is filled in.
- **Clear** empties this pattern: every block comes off the Time row, every
  level goes back to 100 %, and the filter goes off with its Cutoff and Res
  rows put back. Its rate and its length are left as they are, so you keep
  the grid and lose only what was drawn on it.

None of them changes which pattern you are looking at, so after copying
pattern 3 over pattern 9 you are still looking at 3 — click 9 to see the
copy. Each is **one undo step**: if you replace, fill or empty the wrong
pattern, Undo puts it straight back.

### The rate and length of the pattern you are looking at

The bar's second row starts with the two settings that shape the pattern's
grid. Both of them belong to the **pattern you are looking at** — not to the
plugin as a whole — so each of the sixteen can have its own, and both are
saved with the project.

| Control | Values | Default | What it does |
|---|---|---|---|
| **Rate** | 1/8, 1/8T, 1/16, 1/16T, 1/32 | 1/16 | How long one step lasts. `T` is a triplet: 1/8T is two thirds of a 1/8, so three of them fill a quarter note. |
| **Length** | 1 to 64 steps | 16 | How many steps the pattern has. |

Click the rate to pick one from the list. For the length, **drag** the box
left and right, or **click it and type a number** — Enter takes what you
typed, Escape leaves it alone, and holding **Shift** while you drag moves it
a tenth as fast, for picking one step out of the sixty-four. A figure outside
1 to 64 is pulled back to the nearest end rather than refused, so typing 100
gives you 64.

Together these two decide how much song the pattern covers: a pattern spans
its length times its rate, which is why the default 16 steps of 1/16 is
exactly one bar of 4/4. 32 steps of 1/32 is also one bar, at twice the
resolution; 64 steps of 1/8 is eight bars. Changing either one re-times the
whole pattern against the song — see *What a pattern plays* above for how a
pattern lines up with the bar.

**Shortening a pattern throws away whatever falls outside it.** A block that
started past the new last step is gone, and one that straddles it is cut
short. That is one undo step, so you can always get it back.

### Mix and Smoothing

Next along the same row are the two rails that drive **Mix** and
**Smoothing** — the plugin parameters described under *Parameters* above, not
anything belonging to one pattern. Each rail carries its own name at the left
and its value at the right, and fills up from the left as the value rises —
the Smoothing rail says **Smooth**, which is all that fits beside a value.

- **Drag** the rail, or **click** anywhere along it: the value is wherever the
  pointer is, so a click lands on the value under it and a drag follows your
  hand. Dragging past either end gives you exactly that end, so 0 % and 100 %
  are easy to hit.
- **Right-click** the rail to type a value instead. Enter takes what you
  typed, Escape leaves the value alone, and a figure the parameter cannot
  read is simply not taken. Units are optional and the ones Bitwig shows are
  understood: `50`, `50 %`, `4` and `4 ms` all work. A figure outside the
  range is pulled back to the nearest end.

These two belong to Bitwig, which is the whole point of them being parameters:
**moving a rail is recorded as automation** if Bitwig is recording, an
automation lane moving makes the rail move, and **Bitwig's own undo** covers
the move — not this window's.

### Undo and Redo

The row ends with the window's own **Undo** and **Redo** buttons. They step
through the edits you have made to the patterns: picking a rate, changing a
length, clicking a block onto the Time row, moving or stretching one,
choosing what one does, duplicating or deleting one, drawing levels on the
Volume row, and copying or clearing a whole pattern.

- **One step per gesture.** A whole drag comes back at once, not a pixel at a
  time, and a gesture that ended where it started is no step at all.
- A button is **greyed out** when there is nothing for it to do, and says so
  on its hover text.
- Editing something after an undo starts a new future: there is nothing left
  to redo past that point.
- The last hundred steps are kept. Opening a project clears the history —
  stepping back into the patterns it replaced would be a surprise.
- **Ctrl+Z**, **Ctrl+Shift+Z** and **Ctrl+Y** do the same two things from the
  keyboard (see *Keyboard* below).

**Pattern, Mix and Smoothing are not in this history.** Those three are
Bitwig's, and Bitwig's undo covers them.

### On top

Near the end of the row is the **On top** switch, which decides whether this
window stays above Bitwig's.

- **On** — how it starts, and how every new device starts — the desktop keeps
  the window in front of Bitwig: it does not disappear behind the main window
  when you click back into the arranger, and it comes up with Bitwig.
- **Off**, the two windows stack like any other pair: Alt+Tab moves between
  them, and you can put the window beside Bitwig, or behind it, rather than
  over it. Turning it off brings the window to the front once, so it does not
  vanish the moment it is unpinned.

The switch is saved with the project, and it belongs to the device rather
than to stuttter as a whole: one instance can stay in front while another
in the same project does not. It is not part of the window's Undo.

**Turning it back on while the window is open** asks the desktop to hold the
window in front again. Whether it does so straight away is the window
manager's call, not the plugin's — if the window does not go back in front,
close it and open it again and it will.

### Notes

Next on the row is **Notes**: whether notes play the patterns, and from
which key. It reads **Notes off** on a new device; pick a range such as
**C6–D#7** to turn it on, and it then reads **Notes C6**. See
[Playing patterns from notes](#playing-patterns-from-notes) for how to use
it. Like On top it is saved with the project and is not part of the window's
Undo.

### Latch

The last control on the row is **Latch**, greyed out while Notes is off.
Off (the default), a note plays its pattern for as long as it is held; on, a
note starts its pattern and it keeps playing until the next note, and the key
after the sixteen stops it. Saved with the project, like Notes.

### The Time row

Under the bar is the Time row: the steps of the pattern you are looking at,
one cell each, with its blocks drawn over them.

**The whole pattern is always on screen.** Rather than scrolling, the row
divides its width by the pattern's length: at 32 steps the cells are half as
wide as at 16, and at the longest pattern of 64 they are a quarter of it —
narrow, but still plainly cells, and still every one of them clickable. Make
the window wider for more room. There is no scroll bar and nothing to
scroll: change the **Length** and the grid redraws at the new spacing. The Volume row and
the waveform view below it line up with the same columns, so a step, its
level and its sound are always in the same place across the window.

**Alternate beats are shaded** a shade lighter, so you can see where the beat
falls whatever the rate is — four steps to a beat at 1/16, three at 1/8T,
eight at 1/32. The pattern's first beat is the unshaded one.

**A pattern with no blocks on it is a finished pattern.** There is no empty
state to get out of and nothing to set up first: every step of it plays the
input live, and the waveform view at the bottom goes on showing the sound
passing through. A pattern you have just cleared is in exactly that state.

A block is drawn as one coloured piece across the steps it covers, with a
short label on it. Each kind of block has its own colour and its own label,
so a pattern can be read at a glance:

| Label | Block | Colour |
|---|---|---|
| **Rpt 1/16** | Any block with a loop, showing its length — `Rpt 1/1` down to `Rpt 1/64T` | blue |
| **Rev** | Reverse | violet |
| **½×** | Half speed | cyan |
| **2×** | Double speed | green |
| **Stop** | Tape stop | red |
| **Start** | Tape start | yellow |
| **0.5→1.5** | Any other speed ramp, written as its two speeds | pink |

A loop that does not play at normal speed adds its speed: `Rpt 1/8 ×0.5` at
half speed, `Rpt 1/16 1→2` when it ramps. A block that reads from a fixed
spot has a `‹` in front of its label, and one that continues the block
before it has a `…` — `…Stop` is a tape stop picking up where the previous
block left off. A block too narrow for its label is drawn without it and
says what it is by its colour alone.

A block that loops has a thin tick at every repeat, so you can count them: a
1/4 loop over three beats shows two ticks — three times round. The block you
have selected has a white outline.

The pointer changes shape over a block — a hand over its middle, a
horizontal arrow over either end — so you can see what you have hold of
before you press.

#### Adding a block

**Click an empty step and a block appears on it**, one step long, selected
and ready to edit in the block panel. **Press and drag** across empty steps
instead and the new block grows under the pointer, so you can draw a block of
any length in one gesture. Either way it is one undo step, and **Undo** takes
the block away again.

The block you get is **the last kind of block you chose** — so a row of clicks
builds a row of the same block, and you only have to say what you want when
you want something different. Until you have chosen anything, a click adds
the classic stutter: a **Repeat** of one step of the pattern's own rate,
looping from the start of the block. That follows the rate of the pattern you
clicked on, so on a 1/32 pattern you get `Rpt 1/32` and on a 1/16 pattern
`Rpt 1/16`, and either way the block repeats exactly the step it sits on.

Clicking a step a block already covers selects that block — blocks never
overlap.

#### Moving and stretching a block

**Drag a block's middle to move it** along the row, and **drag either end to
stretch or shrink it**. The block follows the pointer a whole step at a time:
the step you picked it up by stays under the pointer as you move it, and a
dragged end snaps to the cell the pointer is in.

- A block is never shorter than **one step**, so pulling an end back through
  the block stops there rather than turning it inside out.
- **Blocks never overlap and never leave the pattern.** A drag stops against
  the neighbour in its way and against the first and last step of the
  pattern, so a block that cannot fit where you are pulling it simply waits
  there — keep pulling and it goes no further, come back and it follows you
  again. An end stretched towards a neighbour fills the gap up to it and
  grows no more: nothing is pushed along, and nothing is overwritten.
- **The whole drag is one undo step**, taken when you let go. A drag that
  ended where it started is no step at all, so a block you picked up and put
  back leaves the history alone.
- You can hear it while you drag: every step of travel is published to the
  audio straight away, so a block dragged while the song plays moves under
  the playhead as you move it.
- Pressing a block also selects it.

The window keeps showing the pattern a drag started on even if the Pattern
parameter changes during it, so the block cannot move out from under your
hand.

#### Choosing what a block does: the block panel

**Click a block** to select it, and the panel on the right shows it: its
label in its colour and the steps it covers, then all of its settings at
once — no menus to open.

- **Preset** — **Repeat**, **Reverse**, **Half speed**, **Double speed**,
  **Tape stop** and **Tape start**, each in its own colour, the block's own
  lit. Click one and the block becomes it (its source is kept). Each says
  what it does to the sound on its hover text.
- **Loop** — **Off**, or a length from **1/1** to **1/64**. The **T** button
  makes the length a triplet.
- **Source** — **Block start**, **Fixed spot** or **Continue**, described
  under *What a block does* above. Clicking **Fixed spot** keeps the block
  sounding the same and pins its source where it is now. With it chosen, a
  **Step** box and a **passes back** box appear under it: drag either, or
  click and type a number. The step is counted from 1, like the panel's
  heading.
- **Speed** — two boxes, the speed at the block's start and at its end. Drag
  either box, or click it and type a number and press Enter; holding
  **Shift** while you drag moves it a tenth as fast, and a figure outside −2
  to +2 is pulled back to the nearest end. You can hear it as you drag, and
  the whole drag is one undo step.

Under the settings a sentence says what the block does — for example
*Repeats 1/4 of audio 3 times, taken from the block's own start, at normal
speed* — and how far back it reaches. And the **waveform view** washes the
stretch of song the block reads from in the block's colour, with the caption
`reads here` (adding *one pass back* when it reaches into the pass before),
so you can see exactly what it is repeating or slowing down. **Drag that box
along the waveform** to move where the block reads from: it snaps to the
steps and turns the source into a fixed spot. On a looping block, **drag
its right edge** (the solid bar) to change the loop length; it snaps to the
nearest note length. Each drag is one undo step.

Moving a speed off one of the named shapes makes it a plain speed ramp: the
block changes colour and its label becomes the two speeds, `1→0.1`. Picking
**Tape stop** again puts the name back.

**Whatever you choose here is what the next click adds.** Choosing anything
for one block sets the block a click on an empty step makes, on this pattern
and on every other, and that choice is saved with the project (except a fixed
spot, which belongs to one block: a new block reads from its own start) — so building
a row of reverses is one click on **Reverse** and then one click per step.

Each change is one undo step.

#### Duplicating and removing a block

The panel ends with two buttons for the block itself:

- **Duplicate** puts a copy in the space right after the block — same
  length, same settings, cut short if the gap is smaller — and selects the
  copy. With no room at all (the block ends where the pattern does, or its
  neighbour is right against it) nothing happens.
- **Remove** takes the block off the row, leaving its steps playing live
  again.

**Right-click a block** to remove it straight away, and **the Delete key**
removes the selected block. **Escape** deselects. Each removal is one undo
step, so **Undo** brings the block back.

### Drawing the Volume row

Under the Time row is the Volume row: one bar per step, on the same columns,
showing the level that step carries. A full bar is full level and an empty
one is silence; every step starts full, so a pattern you have not drawn in
has a row of full bars. The level applies to the effect signal only, and
changes between steps are ramped over **Smoothing** rather than switched, so
drawing a gate pattern here does not click.

**Drag across the row to draw.** The bar under the pointer follows its height
— up for louder, down for quieter — and sweeping sideways draws every bar you
cross, so one diagonal sweep is a ramp across the steps and a flat sweep
levels them all off. A quick sweep that skips over bars fills them in on the
way rather than leaving gaps. Dragging above or below the row keeps drawing at
full or at silence, and dragging off either end stops at the first or last
bar.

- **A click** sets the one bar you clicked to the height you clicked at.
- **Double-click a bar** to put it back to 100 %.
- Levels are whole percent, and the hover text says the step and its level.
- **One undo step per drag**, however many bars it drew, taken when you let
  go. A click is a gesture of its own, so a bar clicked down and then
  double-clicked back takes two presses of **Undo** to return to where it
  started.

**The Cutoff and Res rows** are drawn the same way. Pick one with the tabs
over the row — **Volume**, **Cutoff** or **Res** — and the row shows that one
instead, in its own colour; the hover text says the step's cutoff in Hz or
its resonance in percent. Each window opens on Volume.

- **Double-click a Cutoff bar** to put it back where the filter is open: the
  top for a low-pass, the bottom for a high-pass, the middle for a band-pass.
  On the Res row, double-click puts a bar back to none.
- **Filter**, the dropdown at the right of the tabs, picks this pattern's
  filter: **Off**, **Low-pass**, **High-pass** or **Band-pass**. If you have
  not drawn on the Cutoff row yet, picking a filter moves its bars to where
  that filter is open, so nothing is cut until you draw.
- **Drawing on Cutoff or Res while the filter is Off** turns on Low-pass, as
  part of the same drag.
- While the filter is Off the Cutoff and Res bars are drawn dimmed.
- Picking a filter is one undo step; **Clear** on a pattern's right-click
  menu turns its filter off and puts both rows back.

### The waveform view

The band at the bottom of the window is the sound, not the pattern: it shows
one pass of the pattern, with **what came into the plugin drawn dim grey and
what went out of it drawn bright** on top. Where the effect is passing audio
through untouched the output is light grey and the two sit exactly on top of
each other, so you see one waveform. Under a block the output is drawn **in
that block's colour** (blue under a Repeat, purple under a Reverse, and so
on), so you can see it depart from the grey original behind it, which is
the quickest way to see what a block is actually doing.

It is drawn left to right across the same width as the two rows above it, so
the middle of the view is the middle of the pattern and a step, its level and
its sound are above one another.

**The line is where in the pattern the song is now.** It sweeps left to right
once per pass of the pattern and jumps back to the start when the pattern
loops round.

**Every block's source is marked on it.** The selected block's is washed
over the full height, with its caption, and can be dragged (see *Choosing what a
block does* above). Pointing at a block on the Time row washes its source more lightly.
Every other block's source is a thin stripe in its colour along the bottom;
click a stripe to select that block.

**The part ahead of the line is dimmer**: that is the previous pass, left
where it was drawn rather than wiped as the line goes by, so the view is full
instead of mostly empty. The line sweeping across it replaces it with the
pass that is playing now.

A few things it deliberately does not do:

- **It shows what is playing, not what you are editing.** If you are looking
  at a different pattern from the one Bitwig is playing, the whole view is
  **dimmed** to say so, and the hover text names the pattern it is showing.
  Click the playing pattern's button to watch it live.
- **With Pattern set to Off nothing is drawn**: the view keeps the last pass
  it played, dimmed and held, and the line goes away — there is no "now" to
  mark.
- **Stopping the transport** leaves the last pass on the band with no line
  over it. Stopping or jumping usually also tells the plugin that the audio
  is no longer continuous, which drops the recording the view summarises and
  takes the band down to a flat line; either way it fills up again from the
  first pass after you start playing.
- **Louder than full scale is drawn flat against the top or bottom edge.**
  The view never rescales itself, so a quiet passage looks quiet and a loud
  one looks loud.

Nothing in this band can be clicked or dragged — it is a display.

## Keyboard

The window hands the keys it has no use for back to Bitwig, so you can keep
working with the plugin window focused:

- **Space**, **F1**–**F12** and a letter or a digit held with **Ctrl** go to
  Bitwig — the transport keeps starting and stopping with Space, and Ctrl+S
  still saves the project, as they do everywhere else.
- **While you are typing a figure into a box** — the pattern's length, or a
  speed in the block panel — every key is the box's, Space included, and
  Bitwig sees none of them. Take the figure with Enter, or abandon it with
  Escape, and the keys go where they went before.
- **Ctrl+Z**, **Ctrl+Shift+Z** and **Ctrl+Y** are the exception: the window
  keeps those back for its own undo and redo, and they do exactly what the
  **Undo** and **Redo** buttons on the top bar do — Ctrl+Z steps back,
  Ctrl+Shift+Z and Ctrl+Y step forward. Bitwig never sees them, so its own
  undo is not moved by them either. Holding one down walks through the
  history. There are two times they step nowhere: while you are typing a
  figure into a box, where Ctrl+Z undoes what you have typed rather than the
  gesture before it, and while you are still holding a block or drawing
  levels. Let go first — the drag you let go of is then the step Ctrl+Z
  takes back.
- **Delete** belongs to the window too, and it removes the selected block
  (see *The Time row* above). Bitwig never sees it, so it cannot delete
  anything in the project.
- **Enter**, **Escape**, **Tab**, the arrow keys and plain typing belong to
  the window as well — Escape deselects the block, and Enter and Escape take
  or abandon a number being typed into a box.

## Saving and reopening a project

Saving a Bitwig project stores the three parameter values — Pattern, Mix and
Smoothing — because Bitwig saves a plugin's parameters itself. Reopening the
project brings them back as you left them.

The sixteen patterns are saved with the project as well, in a block of its
own that belongs to the device, and so are the window's own preferences. They
come back when you reopen the project, whether or not you ever open the
plugin window: the patterns are restored either way, and the restored ones
are what plays.

The blocks you click onto the Time row are part of that: a saved project opens
with its patterns as you built them, and with the window's own preferences as
you left them — the **On top** switch, the **Notes** and **Latch** settings,
and the kind of block a click on an empty step adds.

A project file written by a newer version of stuttter, or one that has been
damaged, opens with the patterns it could not read left empty instead of
refusing to load. Projects saved with the earliest test versions, before
blocks had a loop, a source and a speed, open with empty patterns.

## Limits

| | |
|---|---|
| Audio | one stereo input, one stereo output |
| Notes in | off, or 16 keys from any C between C-2 and C7, one per pattern, played while held or latched (then the next key is Stop) |
| Notes out | none |
| Patterns to choose from | 16, plus Off |
| Steps per pattern | 1 to 64 |
| Step lengths | 1/8, 1/8T, 1/16, 1/16T, 1/32 |
| Blocks per pattern | 64 |
| Loop length | 1/1 (a bar) down to 1/64, plus triplets |
| Fixed spot | any step of the pattern, up to 8 extra passes back |
| Speed range | −2 to +2 (double speed backwards to double speed forwards) |
| Step volume | silence to full |
| Filter | off, low-pass, high-pass or band-pass, per pattern |
| Step cutoff | 20 Hz to 20 kHz |
| Step resonance | none to a strong peak (Q 0.7 to 8.5) |
| Mix | 0 % to 100 % |
| Smoothing | 0.5 ms to 20 ms |
| How far back it can read | 64 seconds |
| Latency added to the track | none |
