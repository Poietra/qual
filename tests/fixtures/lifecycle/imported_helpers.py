from external_pkg import boost
from helper_lib import entrance, flourish, make_square, spin, tag
from manim import Scene, Square


class ImportedHelper(Scene):
    def construct(self):
        sq = Square()
        flourish(self, sq)


class ImportedUpdater(Scene):
    def construct(self):
        sq = Square()
        spin(self, sq)


class SceneSecond(Scene):
    def construct(self):
        sq = Square()
        tag(sq, scene=self)


class ImportedFactory(Scene):
    def construct(self):
        sq = make_square()
        self.add(sq)


class ImportedAnimation(Scene):
    def construct(self):
        sq = Square()
        self.add(sq)
        self.play(entrance(sq))


class StarForward(Scene):
    def construct(self):
        args = [Square()]
        flourish(self, *args)


class ThirdParty(Scene):
    def construct(self):
        sq = Square()
        boost(self, sq)
