# The UI

What every click and hold does. This is the spec. `src/ui.rs` implements it and its tests check it, so change this file in the same commit as the code.

The notation is Anduril's, because anyone who owns a TS21 already knows it. **1C** is one click, **2C** two quick clicks, **1H** press and hold, **2H** click then press and hold, and so on. A press becomes a hold after about 375 ms, and clicks count once the switch has been left alone for about 280 ms.

The aim is a smaller UI than Anduril's, with only the parts I use, and every part of it written down here.

## Off

| Action | Does | Status |
|---|---|---|
| 1C | On, at the last level used | Done |
| 1H | On at moonlight (the lowest level). Keep holding and nothing more happens; let go and hold again to ramp up | Done |
| 2C | On at turbo | Done |
| 3C | Battery check, blinking the voltage out | Planned |
| 4C | Lockout | Done |

## On

| Action | Does | Status |
|---|---|---|
| 1C | Off, remembering the level | Done |
| 1H | Ramp up, stopping at the top | Done |
| 2H | Ramp down, stopping at the bottom | Done |
| 2C | Turbo, or back to the level before turbo | Done |

The ramp has 150 levels and goes from bottom to top in about 2.3 seconds (one level per 64 Hz tick).

## Lockout

For pockets and bags.

| Action | Does | Status |
|---|---|---|
| 1H | Moonlight for as long as you hold it | Done |
| 4C | Unlock, back to off | Done |
| Anything else | Nothing | Done |

## Always on, in the background

| What | Does | Status |
|---|---|---|
| Thermal regulation | Steps the level down when the MCU's temperature sensor says the head's too hot, and back up as it cools | Planned |
| Low-voltage protection | Steps down as the cell runs low, then turns off before it's damaged | Planned |
| Sleep when off | Powers down and wakes on the switch, so the light can sit in a drawer for a year | Planned |
| Remember settings | Keeps the memorised level in EEPROM across battery changes | Planned |

Until sleep, thermal regulation and low-voltage protection are in, this isn't firmware to carry. See the [roadmap](roadmap.md).

## Differences from Anduril

- No Simple and Advanced UI split. There's one UI and it's this page.
- 2C from off goes to turbo (Anduril's "2C style 1", which is Wurkkos's default on this light too).
- 1H from off doesn't carry on into a ramp. One hold, one action.

Strobes and blinkies, candle mode, stepped ramping, aux LED patterns, configurable floor and ceiling and a factory-reset gesture might come later.
