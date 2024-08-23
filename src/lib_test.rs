use std::sync::Arc;

use bevy::{
  ecs::system::RunSystemOnce,
  prelude::*,
  scene::ScenePlugin,
  tasks::{AsyncComputeTaskPool, TaskPool},
};
use googletest::prelude::*;

use crate::{
  RegisteredPostProcessActions, ScenePostProcessPlugin, ScenePostProcessTasks,
  ScenePostProcessor,
};

fn create_app() -> App {
  let mut app = App::new();
  app
    .add_plugins(AssetPlugin::default())
    .add_plugins(ScenePlugin)
    .add_plugins(ScenePostProcessPlugin);
  app
}

fn get_scenes(app: &App) -> &Assets<Scene> {
  app.world().resource()
}

fn get_scenes_mut(app: &mut App) -> Mut<Assets<Scene>> {
  app.world_mut().resource_mut()
}

fn get_post_process_tasks(app: &App) -> usize {
  app.world().resource::<ScenePostProcessTasks>().0.len()
}

#[derive(Component)]
struct ExampleMarker;

fn spawn_entity_with_marker_action(world: &mut World, _: &AppTypeRegistry) {
  world.spawn(ExampleMarker);
}

fn scene_contains_entity_with<T: Component>(scene: &mut Scene) -> bool {
  let mut query_state = scene.world.query_filtered::<(), With<T>>();
  // Post processing inserted a new entity with the marker component.
  query_state.iter(&scene.world).len() == 1
}

#[googletest::test]
fn processes_scene_after_loading() {
  let mut app = create_app();
  let task_pool = AsyncComputeTaskPool::get_or_init(|| TaskPool::new());

  let scene_to_process = get_scenes(&app).reserve_handle();

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
  let mut scenes = get_scenes_mut(&mut app);
  expect_that!(scenes.get(&processed_scene), none());

  // Add an empty scene for the original scene.
  scenes.insert(scene_to_process.id(), Scene { world: World::new() });

  app.update();

  // The processed scene has only just started processing, so it's not added
  // yet.
  let scenes = app.world().resource::<Assets<Scene>>();
  expect_that!(scenes.get(&processed_scene), none());
  // There is a task to be processed.
  expect_eq!(get_post_process_tasks(&app), 1);

  task_pool.with_local_executor(|executor| {
    executor.try_tick();
  });

  app.update();

  let mut scenes = get_scenes_mut(&mut app);
  let processed_scene =
    scenes.get_mut(&processed_scene).expect("the scene is finally processed.");
  // Post processing inserted a new entity with the marker component.
  expect_true!(scene_contains_entity_with::<ExampleMarker>(processed_scene));
  // There are no more tasks to be processed.
  expect_eq!(get_post_process_tasks(&app), 0);
}

#[googletest::test]
fn processes_loaded_scene_immediately() {
  let mut app = create_app();
  let task_pool = AsyncComputeTaskPool::get_or_init(|| TaskPool::new());

  let mut scenes = get_scenes_mut(&mut app);
  let scene_to_process = scenes.add(Scene { world: World::new() });

  // Update thrice to make sure all the events are dropped so we don't get saved
  // by a "race condition".
  app.update();
  app.update();
  app.update();
  assert_eq!(app.world().resource::<Events<AssetEvent<Scene>>>().len(), 0);

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

  let mut scenes = get_scenes_mut(&mut app);
  let processed_scene = scenes
    .get_mut(&processed_scene)
    .expect("the processed scene has been loaded.");
  // Post processing inserted a new entity with the marker component.
  expect_true!(scene_contains_entity_with::<ExampleMarker>(processed_scene));
  // There are no more tasks to be processed.
  expect_eq!(get_post_process_tasks(&app), 0);
}

#[googletest::test]
fn multiple_asset_events_only_results_in_one_change() {
  let mut app = create_app();
  let task_pool = AsyncComputeTaskPool::get_or_init(|| TaskPool::new());

  let mut scenes = get_scenes_mut(&mut app);
  let scene_to_process = scenes.add(Scene { world: World::new() });

  // Keep the scene alive even though we don't use it any longer.
  let _processed_scene = {
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

  // Update once to kickoff the processing.
  app.update();
  // Tick the task pool to finish its computation.
  task_pool.with_local_executor(|exec| exec.try_tick());
  // Update again to finish the processing.
  app.update();
  // Update thrice more to clear out all the events.
  app.update();
  app.update();
  app.update();
  assert_eq!(app.world().resource::<Events<AssetEvent<Scene>>>().len(), 0);

  let mut scenes = get_scenes_mut(&mut app);
  let scene = scenes.remove(&scene_to_process).unwrap();
  scenes.insert(&scene_to_process, scene);
  let scene = scenes.remove(&scene_to_process).unwrap();
  scenes.insert(&scene_to_process, scene);

  app.update();

  // There were 4 asset events.
  expect_eq!(app.world().resource::<Events<AssetEvent<Scene>>>().len(), 4);
  // ... but there was only one task created.
  expect_eq!(get_post_process_tasks(&app), 1);

  // Tick the task pool to clear out the task.
  task_pool.with_local_executor(|exec| exec.try_tick());
}

#[googletest::test]
fn drops_post_process_on_drop_output() {
  let mut app = create_app();
  let task_pool = AsyncComputeTaskPool::get_or_init(|| TaskPool::new());

  let mut scenes = get_scenes_mut(&mut app);

  // Keep the scene alive even though we don't use it any longer.
  let processed_scene = {
    let scene_to_process = scenes.add(Scene { world: World::new() });
    app.world_mut().run_system_once(
      move |mut post_processor: ScenePostProcessor| {
        post_processor.process(
          scene_to_process.clone(),
          vec![Arc::new(spawn_entity_with_marker_action)],
        )
      },
    )
  };

  // Update once to kickoff the processing.
  app.update();
  // Tick the task pool to finish its computation.
  task_pool.with_local_executor(|exec| exec.try_tick());
  // Update again to finish the processing.
  app.update();

  drop(processed_scene);

  app.update();
  app.update();

  let scenes = get_scenes(&mut app);
  // All the scenes were dropped.
  expect_eq!(scenes.len(), 0);
  expect_eq!(
    app
      .world()
      .resource::<RegisteredPostProcessActions>()
      .original_to_targets
      .len(),
    0
  );
  expect_eq!(get_post_process_tasks(&app), 0);
}

#[derive(Component)]
struct AnotherMarker;

fn spawn_entity_with_another_marker_action(
  world: &mut World,
  _: &AppTypeRegistry,
) {
  world.spawn(AnotherMarker);
}

#[googletest::test]
fn allows_processing_same_scene_multiple_times() {
  let mut app = create_app();
  let task_pool = AsyncComputeTaskPool::get_or_init(|| TaskPool::new());

  let mut scenes = get_scenes_mut(&mut app);

  let scene_to_process = scenes.add(Scene { world: World::new() });

  let (processed_scene_1, processed_scene_2) = {
    let scene_to_process = scene_to_process.clone();
    app.world_mut().run_system_once(
      move |mut post_processor: ScenePostProcessor| {
        let processed_scene_1 = post_processor.process(
          scene_to_process.clone(),
          vec![Arc::new(spawn_entity_with_marker_action)],
        );
        let processed_scene_2 = post_processor.process(
          scene_to_process.clone(),
          vec![Arc::new(spawn_entity_with_another_marker_action)],
        );
        (processed_scene_1, processed_scene_2)
      },
    )
  };

  // Update once to kickoff the processing.
  app.update();
  // Tick the task pool to finish its computation.
  task_pool.with_local_executor(|exec| exec.try_tick());
  // Update again to finish the processing.
  app.update();

  let mut scenes = get_scenes_mut(&mut app);
  expect_true!(scene_contains_entity_with::<ExampleMarker>(
    scenes.get_mut(&processed_scene_1).expect("The scene was processed."),
  ));
  expect_true!(scene_contains_entity_with::<AnotherMarker>(
    scenes.get_mut(&processed_scene_2).expect("The scene was processed."),
  ));
}
