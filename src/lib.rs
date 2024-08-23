use std::sync::{Arc, Weak};

use bevy::{
  app::{Last, Plugin},
  asset::{AssetEvent, AssetEvents, AssetId, Assets, Handle, StrongHandle},
  ecs::system::SystemParam,
  prelude::{
    AppTypeRegistry, EventReader, IntoSystemConfigs, Res, ResMut, Resource,
    SystemSet, World,
  },
  scene::Scene,
  tasks::{
    block_on, futures_lite::future, tick_global_task_pools_on_main_thread,
    AsyncComputeTaskPool, Task,
  },
  utils::{HashMap, HashSet},
};

/// A plugin to enable post processing scenes.
pub struct ScenePostProcessPlugin;

impl Plugin for ScenePostProcessPlugin {
  fn build(&self, app: &mut bevy::prelude::App) {
    app
      .init_resource::<RegisteredPostProcessActions>()
      .init_resource::<ScenePostProcessTasks>()
      .add_systems(
        Last,
        (
          drop_unused_scenes,
          watch_for_changed_original
            .before(tick_global_task_pools_on_main_thread),
          handle_finished_processing
            .after(tick_global_task_pools_on_main_thread),
        )
          .in_set(PostProcessSet)
          .chain()
          .after(AssetEvents),
      );
  }
}

/// The system set for post processing scenes.
#[derive(SystemSet, PartialEq, Eq, Hash, Debug, Clone)]
pub struct PostProcessSet;

/// A system param for registering post-processing actions.
#[derive(SystemParam)]
pub struct ScenePostProcessor<'w> {
  /// The registered post-processing actions.
  intermediate: ResMut<'w, RegisteredPostProcessActions>,
  /// The currently existing scenes.
  scenes: Res<'w, Assets<Scene>>,
}

impl<'w> ScenePostProcessor<'w> {
  /// Registers a post-processing action on `scene`, which will apply the
  /// `actions` in order. Actions are given a copy of the world contained within
  /// `scene`, and the type registry of the main world (to allow for decoding
  /// components).
  ///
  /// Some things to note:
  ///   - The same scene can be registered multiple times, and each registration
  ///     will post-process its own scene.
  ///   - The original, unprocessed scene will remain loaded even after
  ///     post-processing actions have completed. This allows for listening for
  ///     hot-reloading events.
  ///   - The post-processed scene may not have all the same asset events as the
  ///     original scene. For example, if the unprocessed scene is loaded, then
  ///     unloads and then loads again before the first processing completes, it
  ///     may only have one load event (for the final finished processing). The
  ///     processed scene however should end up matching the original scene's
  ///     state.
  pub fn process(
    &mut self,
    scene: Handle<Scene>,
    actions: Vec<Arc<dyn Fn(&mut World, &AppTypeRegistry) + Send + Sync>>,
  ) -> Handle<Scene> {
    let output_handle = self.scenes.reserve_handle();
    let Handle::Strong(output_strong_handle_arc) = &output_handle else {
      unreachable!("reserve_handle always returns a Handle::Strong");
    };

    if self.scenes.contains(&scene) {
      self.intermediate.new_scenes.insert(scene.id());
    }

    let processing_targets =
      self.intermediate.original_to_targets.entry(scene.id()).or_insert_with(
        move || ProcessingTargets {
          original_handle: scene,
          output_to_action: HashMap::new(),
        },
      );

    processing_targets.output_to_action.insert(
      output_handle.id(),
      PostProcessAction {
        output_handle: Arc::downgrade(output_strong_handle_arc),
        actions,
      },
    );

    output_handle
  }
}

/// The registered actions that will take a scene and create a post-processed
/// scene.
#[derive(Resource, Default)]
struct RegisteredPostProcessActions {
  /// A map from the original scene's asset ID to the targets that should be
  /// updated on changes. We store an [`AssetId`] since this is what
  /// [`AssetEvent`] passes.
  original_to_targets: HashMap<AssetId<Scene>, ProcessingTargets>,
  /// The set of scenes that were added that are immediately ready to be
  /// processed.
  new_scenes: HashSet<AssetId<Scene>>,
}

/// The targets to be updated when a scene is updated.
struct ProcessingTargets {
  /// The handle of the original scene we want to process. Note this must be a
  /// strong handle to keep the asset alive (or else the load could be
  /// cancelled if no original scene handles remain).
  original_handle: Handle<Scene>,
  /// A map from the output scene's asset ID to the action that should be
  /// performed for that asset. We store an [`AssetId`] so we don't keep the
  /// output handle alive here (and because [`Weak<StrongHandle>`] doesn't impl
  /// [`Hash`]).
  output_to_action: HashMap<AssetId<Scene>, PostProcessAction>,
}

/// The action to take when post-processing occurs.
struct PostProcessAction {
  /// The handle where the post-processed scene will be written to. We store a
  /// [`Weak<StrongHandle>`], since:
  ///
  /// 1) we need to be able to determine whether the last reference to the
  ///    asset is gone (so we can drop this action), and
  /// 2) we don't want to keep the processed asset alive.
  ///
  /// We can't use an [`AssetId<Scene>`] since [`Assets<Scene>`] doesn't know
  /// about our asset until we first insert the asset (after we've
  /// post-processed the first time).
  output_handle: Weak<StrongHandle>,
  actions: Vec<Arc<dyn Fn(&mut World, &AppTypeRegistry) + Send + Sync>>,
}

/// The currently running post-processing tasks.
#[derive(Resource, Default)]
struct ScenePostProcessTasks(HashMap<PostProcessKey, Task<Scene>>);

/// The key for a post-processing task.
#[derive(Debug, Hash, PartialEq, Eq)]
struct PostProcessKey {
  /// The [`AssetId`] of the scene we are processing.
  original_id: AssetId<Scene>,
  /// The [`AssetId`] where the processed scene will be written to.
  output_id: AssetId<Scene>,
}

/// System that looks through all the output scene handles, and drops those that
/// are no longer used.
fn drop_unused_scenes(
  mut intermediate: ResMut<RegisteredPostProcessActions>,
  mut post_process_tasks: ResMut<ScenePostProcessTasks>,
) {
  intermediate.original_to_targets.retain(|original_id, targets| {
    targets.output_to_action.retain(|output_id, action| {
      if action.output_handle.strong_count() > 0 {
        return true;
      }

      // We should cancel any existing tasks for this entry. Just let the task
      // drop to cancel it.
      post_process_tasks.0.remove(&PostProcessKey {
        original_id: *original_id,
        output_id: *output_id,
      });

      false
    });

    !targets.output_to_action.is_empty()
  });
}

/// System that reads events for the unprocessed assets and ensures the
/// processing starts or is cleared to match the state of the unprocessed asset.
fn watch_for_changed_original(
  mut scene_events: EventReader<AssetEvent<Scene>>,
  mut intermediate: ResMut<RegisteredPostProcessActions>,
  type_registry: Res<AppTypeRegistry>,
  mut scenes: ResMut<Assets<Scene>>,
  mut post_process_tasks: ResMut<ScenePostProcessTasks>,
) {
  let intermediate = intermediate.as_mut();
  for original_id in scene_events
    .read()
    .filter_map(asset_event_to_change)
    .chain(intermediate.new_scenes.drain().map(|original_id| original_id))
    .collect::<HashSet<_>>()
  {
    let Some(post_process_targets) =
      intermediate.original_to_targets.get(&original_id)
    else {
      // This is not a scene we care about.
      continue;
    };

    if !scenes.contains(original_id) {
      for (&output_id, action) in post_process_targets.output_to_action.iter() {
        // If the original scene is missing, we should also remove the output
        // scene so they remain consistent.
        if let Some(strong_handle) = action.output_handle.upgrade() {
          // If we can upgrade our strong handle, then the scenes Assets may
          // still be holding the handle. Else the asset will have
          // already been cleaned up by the strong handle drop.
          scenes.remove(&Handle::Strong(strong_handle));
        }
        // Just drop the task if it's present to cancel it.
        post_process_tasks.0.remove(&PostProcessKey { original_id, output_id });
      }
      continue;
    }

    let Some(original_scene) =
      scenes.get(&post_process_targets.original_handle)
    else {
      // This could happen if the original scene is loaded and unloaded in the
      // same frame. I'm not sure this is possible, but it's fine if this
      // happens anyway.
      continue;
    };

    for (&output_id, action) in post_process_targets.output_to_action.iter() {
      let Ok(cloned_scene) = original_scene.clone_with(&type_registry) else {
        // TODO: Emit an event here to notify of errors.
        continue;
      };

      let type_registry = type_registry.clone();

      let async_actions = action.actions.clone();

      let task_pool = AsyncComputeTaskPool::get();
      let task = task_pool.spawn(async move {
        let mut processed_scene = cloned_scene;
        for action in async_actions {
          action(&mut processed_scene.world, &type_registry);
        }
        processed_scene
      });
      post_process_tasks
        .0
        .insert(PostProcessKey { original_id, output_id }, task);
    }
  }
}

/// Determines whether to reprocess an asset from an asset event.
fn asset_event_to_change(
  scene_event: &AssetEvent<Scene>,
) -> Option<AssetId<Scene>> {
  match scene_event {
    AssetEvent::Added { id }
    | AssetEvent::Modified { id }
    | AssetEvent::Removed { id } => Some(*id),
    // Not possible for the scenes we care about, since we hold a strong
    // handle to them.
    AssetEvent::Unused { .. } => None,
    AssetEvent::LoadedWithDependencies { .. } => None,
  }
}

/// System that checks for any finished processing tasks and writes their
/// results to the [`Assets<Scene>`] resource.
fn handle_finished_processing(
  mut post_process_tasks: ResMut<ScenePostProcessTasks>,
  intermediate: Res<RegisteredPostProcessActions>,
  mut scenes: ResMut<Assets<Scene>>,
) {
  post_process_tasks.0.retain(|key, task| {
    if !task.is_finished() {
      return true;
    }

    let processed_scene =
      block_on(future::poll_once(task)).expect("the task is finished");

    let Some(action) = intermediate
      .original_to_targets
      .get(&key.original_id)
      .and_then(|targets| targets.output_to_action.get(&key.output_id))
    else {
      // The conversion must have been deleted, so just ignore this as spurious.
      return false;
    };

    // Only insert the scene if we can upgrade the handle (meaning someone still
    // cares about the processed scene).
    if let Some(strong_handle) = action.output_handle.upgrade() {
      scenes.insert(&Handle::Strong(strong_handle), processed_scene);
    }

    false
  });
}

#[cfg(test)]
#[path = "lib_test.rs"]
mod test;
