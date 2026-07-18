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


class DeepFour(Scene):
    def d1(self, mob):
        self.d2(mob)

    def d2(self, mob):
        self.d3(mob)

    def d3(self, mob):
        self.d4(mob)

    def d4(self, mob):
        self.play(FadeIn(mob))

    def construct(self):
        self.d1(Square())


class DeepSix(Scene):
    def s1(self, mob):
        self.s2(mob)

    def s2(self, mob):
        self.s3(mob)

    def s3(self, mob):
        self.s4(mob)

    def s4(self, mob):
        self.s5(mob)

    def s5(self, mob):
        self.s6(mob)

    def s6(self, mob):
        self.play(FadeIn(mob, run_time=0))

    def construct(self):
        self.s1(Square())


class DeepTen(Scene):
    def t1(self, mob):
        self.t2(mob)

    def t2(self, mob):
        self.t3(mob)

    def t3(self, mob):
        self.t4(mob)

    def t4(self, mob):
        self.t5(mob)

    def t5(self, mob):
        self.t6(mob)

    def t6(self, mob):
        self.t7(mob)

    def t7(self, mob):
        self.t8(mob)

    def t8(self, mob):
        self.t9(mob)

    def t9(self, mob):
        self.t10(mob)

    def t10(self, mob):
        self.play(FadeIn(mob))

    def construct(self):
        self.t1(Square())


class MutualPlay(Scene):
    def ping(self, mob):
        self.play(FadeIn(mob))
        self.pong(mob)

    def pong(self, mob):
        self.play(FadeIn(mob))
        self.ping(mob)

    def construct(self):
        self.ping(Square())


class WideChain(Scene):
    def shine(self, mob):
        self.glow(mob)

    def glow(self, mob):
        self.play(FadeIn(mob))

    def construct(self):
        sq = Square()
        self.shine(sq)
        self.shine(sq)
        self.shine(sq)
        self.shine(sq)
        self.shine(sq)
