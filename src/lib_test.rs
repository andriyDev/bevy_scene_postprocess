use std::sync::Arc;

use bevy::{
  ecs::system::RunSystemOnce,
  prelude::*,
  scene::ScenePlugin,
  tasks::{AsyncComputeTaskPool, TaskPool},
};
use googletest::prelude::*;

use crate::{
  ScenePostProcessPlugin, ScenePostProcessTasks, ScenePostProcessor,
};

fn create_app() -> App {
  let mut app = App::new();
  app
    .add_plugins(AssetPlugin::default())
    .add_plugins(ScenePlugin)
    .add_plugins(ScenePostProcessPlugin);
  app
}

#[derive(Component)]
struct ExampleMarker;

fn spawn_entity_with_marker_action(world: &mut World) {
  world.spawn(ExampleMarker);
}

#[googletest::test]
fn processes_scene_after_loading() {
  let mut app = create_app();
  let task_pool = AsyncComputeTaskPool::get_or_init(|| TaskPool::new());

  let scenes = app.world_mut().resource_mut::<Assets<Scene>>();
  let scene_to_process = scenes.reserve_handle();

  let processed_scene = {
    let scene_to_process = scene_to_process.clone();
    app.world_mut().run_system_once(
      move |mut post_processor: ScenePostProcessor| {
        post_processor.process(
          scene_to_process.clone(),
          vec![Arc::new(spawn_entity_with_marker_action)],
        )
      },
    )
  };

  app.update();

  // The processed scene hasn't been added yet.
  let mut scenes = app.world_mut().resource_mut::<Assets<Scene>>();
  expect_that!(scenes.get(&processed_scene), none());

  // Add an empty scene for the original scene.
  scenes.insert(scene_to_process.id(), Scene { world: World::new() });

  app.update();

  // The processed scene has only just started processing, so it's not added
  // yet.
  let scenes = app.world().resource::<Assets<Scene>>();
  expect_that!(scenes.get(&processed_scene), none());
  // There is a task to be processed.
  expect_that!(&app.world().resource::<ScenePostProcessTasks>().0, len(eq(1)));

  task_pool.with_local_executor(|executor| {
    executor.try_tick();
  });

  app.update();

  let mut scenes = app.world_mut().resource_mut::<Assets<Scene>>();
  let processed_scene =
    scenes.get_mut(&processed_scene).expect("the scene is finally processed.");
  let mut query_state =
    processed_scene.world.query_filtered::<(), With<ExampleMarker>>();
  // Post processing inserted a new entity with the marker component.
  expect_that!(query_state.iter(&processed_scene.world).len(), eq(1));
  // There are no more tasks to be processed.
  expect_that!(&app.world().resource::<ScenePostProcessTasks>().0, len(eq(0)));
}

#[googletest::test]
fn processes_loaded_scene_immediately() {
  let mut app = create_app();
  let task_pool = AsyncComputeTaskPool::get_or_init(|| TaskPool::new());

  let mut scenes = app.world_mut().resource_mut::<Assets<Scene>>();
  let scene_to_process = scenes.add(Scene { world: World::new() });

  // Update twice to make sure all the events are dropped so we don't get saved
  // by a "race condition".
  app.update();
  app.update();

  let processed_scene = app.world_mut().run_system_once(
    move |mut post_processor: ScenePostProcessor| {
      post_processor.process(
        scene_to_process.clone(),
        vec![Arc::new(spawn_entity_with_marker_action)],
      )
    },
  );

  // Let the processor "see" the new processed scene.
  app.update();

  // Let the task pool finish its computation.
  task_pool.with_local_executor(|exec| {
    exec.try_tick();
  });

  // Let the processor handle the finished processing.
  app.update();

  let mut scenes = app.world_mut().resource_mut::<Assets<Scene>>();
  let processed_scene = scenes
    .get_mut(&processed_scene)
    .expect("the processed scene has been loaded.");
  let mut query_state =
    processed_scene.world.query_filtered::<(), With<ExampleMarker>>();
  // Post processing inserted a new entity with the marker component.
  expect_that!(query_state.iter(&processed_scene.world).len(), eq(1));
  // There are no more tasks to be processed.
  expect_that!(&app.world().resource::<ScenePostProcessTasks>().0, len(eq(0)));
}