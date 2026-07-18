from manim import FadeIn


def flourish(scene, mob):
    scene.play(FadeIn(mob, run_time=0))
