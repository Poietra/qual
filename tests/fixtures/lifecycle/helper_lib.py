from manim import FadeIn, RIGHT, Square


def flourish(scene, mob):
    scene.play(FadeIn(mob), run_time=2)


def wiggle(scene, mob):
    scene.play(mob.animate.shift(RIGHT), run_time=2)


def spin(scene, mob):
    mob.add_updater(lambda m, dt: m.rotate(dt))
    scene.add(mob)


def tag(mob, scene):
    scene.play(FadeIn(mob))


def make_square():
    return Square()


def entrance(mob):
    return FadeIn(mob)
