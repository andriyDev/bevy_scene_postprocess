use std::sync::{Arc, Weak};

use bevy::{
  app::{Last, Plugin},
  asset::{AssetEvent, AssetEvents, AssetId, Assets, Handle, StrongHandle},
  ecs::system::SystemParam,
  prelude::{
    AppTypeRegistry, EventReader, IntoSystemConfigs, Res, ResMut, Resource,
    World,
  },
  scene::Scene,
  tasks::{
    block_on, futures_lite::future, tick_global_task_pools_on_main_thread,
    AsyncComputeTaskPool, Task,
  },
  utils::{HashMap, HashSet},
};

pub struct ScenePostProcessPlugin;

impl Plugin for ScenePostProcessPlugin {
  fn build(&self, app: &mut bevy::prelude::App) {
    app
      .init_resource::<ScenePostProcessIntermediate>()
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
          .chain()
          .after(AssetEvents),
      );
  }
}

#[derive(SystemParam)]
pub struct ScenePostProcessor<'w> {
  intermediate: ResMut<'w, ScenePostProcessIntermediate>,
  scenes: Res<'w, Assets<Scene>>,
}

impl<'w> ScenePostProcessor<'w> {
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

    self.intermediate.original_to_post_process.insert(
      scene.id(),
      PostProcessAction {
        original_handle: scene,
        output_handle: Arc::downgrade(output_strong_handle_arc),
        output_id: output_handle.id(),
        actions,
      },
    );

    output_handle
  }
}

#[derive(Resource, Default)]
struct ScenePostProcessIntermediate {
  original_to_post_process: HashMap<AssetId<Scene>, PostProcessAction>,
  new_scenes: HashSet<AssetId<Scene>>,
}

struct PostProcessAction {
  original_handle: Handle<Scene>,
  output_handle: Weak<StrongHandle>,
  output_id: AssetId<Scene>,
  actions: Vec<Arc<dyn Fn(&mut World, &AppTypeRegistry) + Send + Sync>>,
}

#[derive(Resource, Default)]
struct ScenePostProcessTasks(HashMap<PostProcessKey, Task<Scene>>);

#[derive(Debug, Hash, PartialEq, Eq)]
struct PostProcessKey {
  original_id: AssetId<Scene>,
  output_id: AssetId<Scene>,
}

fn drop_unused_scenes(
  mut intermediate: ResMut<ScenePostProcessIntermediate>,
  mut post_process_tasks: ResMut<ScenePostProcessTasks>,
) {
  intermediate.original_to_post_process.retain(|original, action| {
    if action.output_handle.strong_count() > 0 {
      return true;
    }

    // We should cancel any existing tasks for this entry. Just let the task
    // drop to cancel it.
    post_process_tasks.0.remove(&PostProcessKey {
      original_id: *original,
      output_id: action.output_id,
    });

    false
  });
}

fn watch_for_changed_original(
  mut scene_events: EventReader<AssetEvent<Scene>>,
  mut intermediate: ResMut<ScenePostProcessIntermediate>,
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
    let Some(post_process_action) =
      intermediate.original_to_post_process.get(&original_id)
    else {
      // This is not a scene we care about.
      continue;
    };

    if !scenes.contains(original_id) {
      // If the original scene is missing, we should also remove the output
      // scene so they remain consistent.
      if let Some(strong_handle) = post_process_action.output_handle.upgrade() {
        // If we can upgrade our strong handle, then the scenes Assets may still
        // be holding the handle. Else the asset will have already been cleaned
        // up by the strong handle drop.
        scenes.remove(&Handle::Strong(strong_handle));
      }
      // Just drop the task if it's present to cancel it.
      post_process_tasks.0.remove(&PostProcessKey {
        original_id,
        output_id: post_process_action.output_id,
      });
      continue;
    }

    let Some(original_scene) = scenes.get(&post_process_action.original_handle)
    else {
      // This could happen if the original scene is loaded and unloaded in the
      // same frame. I'm not sure this is possible, but it's fine if this
      // happens anyway.
      continue;
    };

    let Ok(cloned_scene) = original_scene.clone_with(&type_registry) else {
      // TODO: Emit an event here to notify of errors.
      continue;
    };

    let type_registry = type_registry.clone();

    let async_actions = post_process_action.actions.clone();

    let task_pool = AsyncComputeTaskPool::get();
    let task = task_pool.spawn(async move {
      let mut processed_scene = cloned_scene;
      for action in async_actions {
        action(&mut processed_scene.world, &type_registry);
      }
      processed_scene
    });
    post_process_tasks.0.insert(
      PostProcessKey { original_id, output_id: post_process_action.output_id },
      task,
    );
  }
}

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

fn handle_finished_processing(
  mut post_process_tasks: ResMut<ScenePostProcessTasks>,
  intermediate: Res<ScenePostProcessIntermediate>,
  mut scenes: ResMut<Assets<Scene>>,
) {
  post_process_tasks.0.retain(|id, task| {
    if !task.is_finished() {
      return true;
    }

    let processed_scene =
      block_on(future::poll_once(task)).expect("the task is finished");

    let Some(post_process_action) =
      intermediate.original_to_post_process.get(&id.original_id)
    else {
      // The conversion must have been deleted, so just ignore this as spurious.
      return false;
    };

    // Only insert the scene if we can upgrade the handle (meaning someone still
    // cares about the processed scene).
    if let Some(strong_handle) = post_process_action.output_handle.upgrade() {
      scenes.insert(&Handle::Strong(strong_handle), processed_scene);
    }

    false
  });
}

#[cfg(test)]
#[path = "lib_test.rs"]
mod test;
