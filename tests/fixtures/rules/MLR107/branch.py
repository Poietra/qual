import manim


def build_scene(scene_cls=None):
    chosen = scene_cls or manim.MovingCameraScene
    # The callee identity is unresolved: silence.
    return chosen()
