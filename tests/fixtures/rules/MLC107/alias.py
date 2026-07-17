from manim import MoveToTarget as Target, Scene as Sc, Square as Sq


class Demo(Sc):
    def construct(self):
        sq = Sq()
        self.add(sq)
        self.play(Target(sq))
