# FAQ

**Is this a Pokémon bot?**
No. `pikapro.rl` is an environment, like poke-env. The features,
network, algorithm and curriculum are yours. The built-in opponents are
poke-env's baseline players, in Rust.

**Which formats?**
`pikapro.formats()`; see [FORMATS.md](../FORMATS.md).

**How close is it to Pokémon Showdown?**
Random battles are replayed on a pinned Showdown and compared line by
line and draw by draw ([CONFORMANCE.md](../CONFORMANCE.md)).

**Why was my action rejected (`status` -1)?**
It was outside the player's mask, or, with factored masks, the two
slots' actions don't pair. Nothing was submitted, and the player is
asked again. `env.pair_mask` gives the second slot's legal actions for
a first-slot action.

**What does `status` 2 (unavailable) mean?**
The battle refused the action for a reason the request hides, such as a
trap by a foe's unrevealed ability. The player's view learns the fact,
and the player chooses again ([honest masks](concepts.md#honest-masks)).

**Why is player 1's observation all zeros?**
A built-in opponent isn't encoded unless you pass
`observe_builtin=True`.

**My stored observations changed after the next step. Why?**
A step returns the environment's own buffers and the next step
overwrites them. Copy what you keep (`np.array(out.obs)`).

**Can I use the GPU?**
The environment runs on the CPU and writes numpy arrays; bind numpy views
of pinned torch tensors to move them cheaply
([performance](performance.md#buffers-and-copies)). The simulator is not
jittable, so there is no JAX version.

**Are results reproducible?**
Within a version, yes. The same seed, teams and actions give the same
battles, for any number of threads and on every platform. Across
versions, a mechanics fix can change battles, and a version bump
announces a change of the action indexing
([layout versions](encoders.md#layout-versions)). Record the package
version with your results.

**How do I watch a battle, or get its Showdown log?**
Create the env with `record=True`, then `rl.replay(format,
env.record(i))` gives a `pikapro.Battle` whose `log("p1")` is player 1's
channel. `cargo run -p pikapro-cli -- play` prints random battles.

**Can my agent play against others?**
Yes. `pikapro.showdown` plays a policy on a local server, a server you
run, or VGC Arena ([playing on a server](showdown.md)). poke-env players
also work, on `pikapro-showdown-server` or in-process
([poke-env](poke-env.md)).

**Can I train on specific teams?**
Yes ([team pools](concepts.md#team-pools)).

**Does it support free-threaded Python (3.13t, 3.14t)?**
Not yet. The wheels use the stable ABI of the regular (GIL) builds.

**Why can't I subclass `Battle` or `VecEnv`'s internals?**
The extension classes are final. Wrap them instead; `pikapro.rl.VecEnv`
itself is a Python class you can subclass.

**Where do I report a bug?**
In the repository's issue tracker. `env.record(i)` collects the seed,
teams and actions that reproduce a battle.
