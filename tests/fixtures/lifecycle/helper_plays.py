import manim as mn
from manim import Circle, FadeIn, Scene, Square


class HelperPlay(Scene):
    def show(self, mob):
        self.play(FadeIn(mob, run_time=0))

    def construct(self):
        self.show(Square())


class TwoCalls(Scene):
    def flash(self, mob):
        self.play(FadeIn(mob), run_time=2)

    def construct(self):
        a = Square()
        b = Circle()
        self.flash(a)
        self.flash(b)


class BranchCall(Scene):
    def maybe_show(self, mob):
        self.play(FadeIn(mob))

    def construct(self):
        sq = Square()
        if unknowable():
            self.maybe_show(sq)


class ChainCall(Scene):
    def outer(self, mob):
        self.inner(mob)

    def inner(self, mob):
        self.play(FadeIn(mob))

    def construct(self):
        self.outer(Square())


class RecursivePlay(Scene):
    def ping(self, mob):
        self.play(FadeIn(mob))
        self.ping(mob)

    def construct(self):
        self.ping(Square())


def flourish(scene, mob):
    scene.play(FadeIn(mob))


class ModuleHelper(Scene):
    def construct(self):
        sq = Square()
        flourish(self, sq)


class WaitHelper(Scene):
    def rest(self):
        self.wait(2)

    def construct(self):
        sq = Square()
        sq.add_updater(lambda m, dt: m.rotate(dt))
        self.add(sq)
        self.rest()


class AliasHelper(mn.Scene):
    def show(self, mob):
        self.play(mn.FadeIn(mob, run_time=0))

    def construct(self):
        self.show(mn.Square())
