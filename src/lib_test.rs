use std::{
  result::Result,
  sync::{Arc, Mutex},
};

use bevy::{
  ecs::system::RunSystemOnce, prelude::*, scene::ScenePlugin,
  tasks::AsyncComputeTaskPool,
};
use googletest::prelude::*;

use crate::{
  BoxedError, RegisteredPostProcessActions, ScenePostProcessPlugin,
  ScenePostProcessTasks, ScenePostProcessor,
};

fn create_app() -> App {
  let mut app = App::new();
  app
    .add_plugins(AssetPlugin::default())
    .add_plugins(ScenePlugin)
    .add_plugins(TaskPoolPlugin::default())
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

fn wait_for_tasks_to_finish(app: &App) {
  let task_pool = AsyncComputeTaskPool::get();

  let app_has_unfinished_tasks = || {
    app
      .world()
      .resource::<ScenePostProcessTasks>()
      .0
      .iter()
      .any(|(_, task)| !task.is_finished())
  };

  while app_has_unfinished_tasks() {
    task_pool.with_local_executor(|exec| {
      exec.try_tick();
    });
  }
}

fn finish_processing_tasks(app: &mut App) {
  // Start processing.
  app.update();
  wait_for_tasks_to_finish(app);
  // Finish processing.
  app.update();
}

#[derive(Component)]
struct ExampleMarker;

fn spawn_entity_with_marker_action(
  world: &mut World,
) -> Result<(), BoxedError> {
  world.spawn(ExampleMarker);

  Ok(())
}

fn scene_contains_entity_with<T: Component>(scene: &mut Scene) -> bool {
  let mut query_state = scene.world.query_filtered::<(), With<T>>();
  // Post processing inserted a new entity with the marker component.
  query_state.iter(&scene.world).len() == 1
}

#[googletest::test]
fn processes_scene_after_loading() {
  let mut app = create_app();

  let scene_to_process = get_scenes(&app).reserve_handle();

  let (sender, receiver) = crossbeam_channel::bounded::<()>(1);
  // Arc the receiver so we can clone it into each closure.
  let receiver = Arc::new(receiver);

  let processed_scene = {
    let scene_to_process = scene_to_process.clone();
    app
      .world_mut()
      .run_system_once(move |mut post_processor: ScenePostProcessor| {
        let receiver = receiver.clone();
        post_processor.process(
          scene_to_process.clone(),
          vec![Arc::new(move |world: &mut World| {
            receiver.recv().unwrap();

            spawn_entity_with_marker_action(world)
          })],
        )
      })
      .unwrap()
  };

  app.update();

  // The processed scene hasn't been added yet, since the unprocessed scene
  // hasn't been loaded.
  let mut scenes = get_scenes_mut(&mut app);
  expect_that!(scenes.get(&processed_scene), none());

  // Add an empty scene for the unprocessed scene.
  scenes.insert(scene_to_process.id(), Scene { world: World::new() });

  app.update();

  // The processed scene has only just started processing, so it's not added
  // yet.
  let scenes = app.world().resource::<Assets<Scene>>();
  expect_that!(scenes.get(&processed_scene), none());
  // There is a task to be processed.
  expect_eq!(get_post_process_tasks(&app), 1);

  // Let the task finish processing.
  sender.send(()).unwrap();
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

  let mut scenes = get_scenes_mut(&mut app);
  let scene_to_process = scenes.add(Scene { world: World::new() });

  // Update so the events are processed by the scene processor (even though they
  // will just be ignored).
  app.update();
  // Clear out the events to make sure these events can't affect processing.
  // This is not necessary but it makes it clear the events aren't related here.
  app.world_mut().resource_mut::<Events<AssetEvent<Scene>>>().clear();

  let processed_scene = app
    .world_mut()
    .run_system_once(move |mut post_processor: ScenePostProcessor| {
      post_processor.process(
        scene_to_process.clone(),
        vec![Arc::new(spawn_entity_with_marker_action)],
      )
    })
    .unwrap();

  finish_processing_tasks(&mut app);

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
fn drops_processed_scene_if_unprocessed_is_dropped() {
  let mut app = create_app();

  let mut scenes = get_scenes_mut(&mut app);
  let scene_to_process = scenes.add(Scene { world: World::new() });

  let processed_scene = {
    let scene_to_process = scene_to_process.clone();
    app
      .world_mut()
      .run_system_once(move |mut post_processor: ScenePostProcessor| {
        post_processor.process(
          scene_to_process.clone(),
          vec![Arc::new(spawn_entity_with_marker_action)],
        )
      })
      .unwrap()
  };

  finish_processing_tasks(&mut app);

  let mut scenes = get_scenes_mut(&mut app);
  expect_that!(scenes.get(&processed_scene), some(anything()));

  // Removing the unprocessed scene should also remove the processed scene.
  scenes.remove(&scene_to_process);

  // Update to let the plugin react.
  app.update();

  let scenes = get_scenes_mut(&mut app);
  expect_that!(scenes.get(&processed_scene), none());
}

#[googletest::test]
fn multiple_asset_events_only_results_in_one_change() {
  let mut app = create_app();

  let mut scenes = get_scenes_mut(&mut app);
  let scene_to_process = scenes.add(Scene { world: World::new() });

  let (sender, receiver) = crossbeam_channel::bounded::<()>(1);
  // Arc the receiver so we can clone it into each closure.
  let receiver = Arc::new(receiver);

  // Keep the scene alive even though we don't use it any longer.
  let _processed_scene = {
    let scene_to_process = scene_to_process.clone();
    app.world_mut().run_system_once(
      move |mut post_processor: ScenePostProcessor| {
        let receiver = receiver.clone();
        post_processor.process(
          scene_to_process.clone(),
          vec![Arc::new(move |_: &mut World| {
            receiver.recv().unwrap();
            Ok(())
          })],
        )
      },
    )
  };

  // Tell the task it is allowed to finish.
  sender.send(()).unwrap();
  finish_processing_tasks(&mut app);
  // Update once more to flush the internal asset events from inserting the
  // processed asset.
  app.update();

  // Clear out the events to make sure these events can't affect processing.
  // This is not necessary but it makes it clear the events aren't related here.
  app.world_mut().resource_mut::<Events<AssetEvent<Scene>>>().clear();

  let mut scenes = get_scenes_mut(&mut app);
  let scene = scenes.remove(&scene_to_process).unwrap();
  scenes.insert(&scene_to_process, scene);
  let scene = scenes.remove(&scene_to_process).unwrap();
  scenes.insert(&scene_to_process, scene);

  app.update();

  // There were 4 asset events from changes to the scene.
  let mut events = app.world_mut().resource_mut::<Events<AssetEvent<Scene>>>();
  expect_eq!(events.len(), 4);
  // Clear out the events so we don't double count them.
  events.clear();
  assert_eq!(get_post_process_tasks(&app), 1);

  // Tell the task it is allowed to finish.
  sender.send(()).unwrap();

  // Update again to hopefully clear out the task pool.
  finish_processing_tasks(&mut app);

  // There was one event from the processed scene loading.
  expect_eq!(app.world().resource::<Events<AssetEvent<Scene>>>().len(), 1);
}

#[googletest::test]
fn drops_post_process_on_drop_output() {
  let mut app = create_app();

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

  finish_processing_tasks(&mut app);

  drop(processed_scene);

  // Update once to handle dropping the processed scene.
  app.update();
  // Update again so the asset system handles the dropped unprocessed scene.
  app.update();

  let scenes = get_scenes(&mut app);
  // All the scenes were dropped.
  expect_eq!(scenes.len(), 0, "{:?}", scenes.ids().collect::<Vec<_>>());
  expect_eq!(
    app
      .world()
      .resource::<RegisteredPostProcessActions>()
      .unprocessed_to_targets
      .len(),
    0
  );
  expect_eq!(get_post_process_tasks(&app), 0);
}

#[derive(Component)]
struct AnotherMarker;

fn spawn_entity_with_another_marker_action(
  world: &mut World,
) -> Result<(), BoxedError> {
  world.spawn(AnotherMarker);

  Ok(())
}

#[googletest::test]
fn allows_processing_same_scene_multiple_times() {
  let mut app = create_app();

  let mut scenes = get_scenes_mut(&mut app);

  let scene_to_process = scenes.add(Scene { world: World::new() });

  let (processed_scene_1, processed_scene_2) = {
    let scene_to_process = scene_to_process.clone();
    app
      .world_mut()
      .run_system_once(move |mut post_processor: ScenePostProcessor| {
        let processed_scene_1 = post_processor.process(
          scene_to_process.clone(),
          vec![Arc::new(spawn_entity_with_marker_action)],
        );
        let processed_scene_2 = post_processor.process(
          scene_to_process.clone(),
          vec![Arc::new(spawn_entity_with_another_marker_action)],
        );
        (processed_scene_1, processed_scene_2)
      })
      .unwrap()
  };

  finish_processing_tasks(&mut app);

  let mut scenes = get_scenes_mut(&mut app);
  expect_true!(scene_contains_entity_with::<ExampleMarker>(
    scenes.get_mut(&processed_scene_1).expect("The scene was processed."),
  ));
  expect_true!(scene_contains_entity_with::<AnotherMarker>(
    scenes.get_mut(&processed_scene_2).expect("The scene was processed."),
  ));
}

#[googletest::test]
fn error_causes_failed_load() {
  let mut app = create_app();

  let mut scenes = get_scenes_mut(&mut app);
  let scene_to_process = scenes.add(Scene { world: World::new() });

  let should_fail = Arc::new(Mutex::new(false));

  let processed_scene = {
    let should_fail = should_fail.clone();

    let action = Arc::new(move |world: &mut World| -> Result<(), BoxedError> {
      if *should_fail.lock().unwrap() {
        return Err("Some message".into());
      }

      spawn_entity_with_marker_action(world)
    });

    let scene_to_process = scene_to_process.clone();

    app
      .world_mut()
      .run_system_once(move |mut post_processor: ScenePostProcessor| {
        post_processor.process(scene_to_process.clone(), vec![action.clone()])
      })
      .unwrap()
  };

  finish_processing_tasks(&mut app);

  expect_eq!(get_post_process_tasks(&app), 0);
  let scenes = get_scenes(&mut app);
  // The scene was processed successfully.
  expect_that!(scenes.get(&processed_scene), some(anything()));

  // Reprocessing the asset now will fail.
  *should_fail.lock().unwrap() = true;

  let mut scenes = get_scenes_mut(&mut app);
  // Get the scene mutably to trigger a modified asset event to trigger
  // reprocessing.
  let _ = scenes.get_mut(&scene_to_process);

  finish_processing_tasks(&mut app);

  expect_eq!(get_post_process_tasks(&app), 0);
  let scenes = get_scenes(&mut app);
  // The stale scene was deleted because the new scene failed to be processed.
  expect_that!(scenes.get(&processed_scene), none());

  // Reprocessing the asset now will pass.
  *should_fail.lock().unwrap() = false;

  let mut scenes = get_scenes_mut(&mut app);
  // Get the scene mutably to trigger a modified asset event to trigger
  // reprocessing.
  let _ = scenes.get_mut(&scene_to_process);

  finish_processing_tasks(&mut app);

  expect_eq!(get_post_process_tasks(&app), 0);
  let scenes = get_scenes(&mut app);
  // The scene was reprocessed successfully!
  expect_that!(scenes.get(&processed_scene), some(anything()));
}

#[derive(Component)]
struct MyUnregisteredComponent;

#[googletest::test]
fn failed_to_clone_scene_removes_all_processed_scenes() {
  let mut app = create_app();

  let mut scenes = get_scenes_mut(&mut app);
  let scene_to_process = scenes.add(Scene { world: World::new() });

  let (processed_scene_1, processed_scene_2) = {
    let scene_to_process = scene_to_process.clone();
    app
      .world_mut()
      .run_system_once(move |mut post_processor: ScenePostProcessor| {
        let processed_scene_1 = post_processor.process(
          scene_to_process.clone(),
          vec![Arc::new(spawn_entity_with_marker_action)],
        );
        let processed_scene_2 = post_processor.process(
          scene_to_process.clone(),
          vec![Arc::new(spawn_entity_with_another_marker_action)],
        );
        (processed_scene_1, processed_scene_2)
      })
      .unwrap()
  };

  finish_processing_tasks(&mut app);

  expect_eq!(get_post_process_tasks(&app), 0);
  let scenes = get_scenes(&mut app);
  // The scenes were processed successfully.
  expect_that!(scenes.get(&processed_scene_1), some(anything()));
  expect_that!(scenes.get(&processed_scene_2), some(anything()));

  let mut scenes = get_scenes_mut(&mut app);
  *scenes.get_mut(&scene_to_process).unwrap() = {
    let mut scene = Scene { world: World::new() };
    scene.world.spawn(MyUnregisteredComponent);
    scene
  };

  // Start processing, which should fail.
  app.update();

  expect_eq!(get_post_process_tasks(&app), 0);
  let scenes = get_scenes(&mut app);
  // The scenes were both removed since cloning the unprocessed scene failed.
  expect_that!(scenes.get(&processed_scene_1), none());
  expect_that!(scenes.get(&processed_scene_2), none());
}
