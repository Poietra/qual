from manim import Restore as Rewind, Scene as Sc, Square as Sq


class Demo(Sc):
    def construct(self):
        sq = Sq()
        self.add(sq)
        self.play(Rewind(sq))
