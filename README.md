# bevy_scene_postprocess

A crate for Bevy that provides a way to "postprocess" scenes after loading them.

## Why?

Scenes are great, but they have a big problem: they are generally loaded from
GLTFs. This means the expressiveness of scenes is limited by GLTF. Crates like
[Blenvy](https://github.com/kaosat-dev/Blenvy) make this nicer by providing ways
to manipulate spawned scenes (in Blenvy's case, by decoding `GltfExtra`s into
components).

While this is great, there is an awkward problem. This requires you to spawn
scenes, and then wait for the newly spawned scene to be processed. This means
that any changes to the scenes (e.g., decoding `GltfExtra`s) need to be repeated
every time you spawn a scene. In addition, this approach doesn't play well with
spawning in scenes synchronously.

With `bevy_scene_postprocess`, you just register your post-processing actions,
and it will only assign the scene once all actions have been completed. This
means you only do your actions once when you load your app, and from then on
Bevy just spawns scenes with nothing special.

Another similar crate is
[`bevy_scene_hook`](https://github.com/nicopap/bevy-scene-hook), but this
performs actions after spawning completes. Note even with post-processing,
[`bevy_scene_hook`](https://github.com/nicopap/bevy-scene-hook) still has a
place. Perhaps you want to do per-object actions that don't make sense to "bake
in" with post-processing.

## How to use

1) Load a scene the way you normally would.
2) Call `ScenePostProcessor::process`.
3) Use the returned handle as you would normally (e.g., in a `SceneBundle`).

## Example

The following example spawns a scene once which only contains `Bar`:

```rust
use std::{
  sync::Arc, time::Duration
};

use bevy::{
  prelude::*, ecs::system::RunSystemOnce, scene::ScenePlugin,
  time::common_conditions::once_after_delay
};
use bevy_scene_postprocess::{ScenePostProcessPlugin, ScenePostProcessor};

fn main() {
  App::new()
    .add_plugins(MinimalPlugins)
    .add_plugins(AssetPlugin::default())
    .add_plugins(ScenePlugin)
    .add_plugins(ScenePostProcessPlugin)
    .add_systems(Startup, register_post_process)
    .add_systems(
      Update,
      fake_load_scene.run_if(once_after_delay(Duration::from_secs_f32(0.1))),
    )
    .add_systems(
      Update,
      (
        move |mut exit: EventWriter<AppExit>| {
          // Quite after loading to let the doctests pass.
          exit.send(AppExit::Success);
        }
      ).run_if(once_after_delay(Duration::from_secs_f32(0.5))),
    )
    .run();
}

fn register_post_process(
  scenes: Res<Assets<Scene>>,
  mut post_processor: ScenePostProcessor,
  mut commands: Commands
) {
  // Pretend we loaded an asset here (loading an asset for real would break
  // doctests).
  let handle = scenes.reserve_handle();
  commands.insert_resource(LoadingHandle(handle.clone()));

  let processed_scene = post_processor.process(handle, vec![
    Arc::new(move |world: &mut World| {
      // For example convenience, just run a system.
      world.run_system_once(replace_foos_with_bar);
      Ok(())
    })
  ]);

  commands.spawn(SceneRoot(processed_scene));
}

#[derive(Resource)]
struct LoadingHandle(Handle<Scene>);

#[derive(Component)]
struct Foo;

#[derive(Component)]
struct Bar;

fn replace_foos_with_bar(foos: Query<Entity, With<Foo>>, mut commands: Commands) {
  for entity in foos.iter() {
    commands.entity(entity).remove::<Foo>().insert(Bar);
  }
}

fn fake_load_scene(
  handle: Res<LoadingHandle>,
  mut scenes: ResMut<Assets<Scene>>
) {
  let mut new_scene = Scene {
    world: World::new(),
  };

  // Spawn three entities with `Foo`. They will be replaced with `Bar` by the
  // post processing.
  new_scene.world.spawn(Foo);
  new_scene.world.spawn(Foo);
  new_scene.world.spawn(Foo);

  scenes.insert(&handle.0, new_scene);
}
```

## License

License under either of

* Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or <http://www.apache.org/licenses/LICENSE-2.0>)
* MIT license ([LICENSE-MIT](LICENSE-MIT) or <http://opensource.org/licenses/MIT>)

at your option.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall
be dual licensed as above, without any additional terms or conditions.
